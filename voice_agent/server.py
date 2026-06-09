"""FastAPI relay between the browser and Azure OpenAI Realtime — slide creator.

Topology:
  browser  <--(ws, JSON)-->  /ws  <--(wss, JSON)-->  Azure Realtime

The API key lives only on this server. The browser never sees it.

On session start:
  1. Accept browser WS (no patient/login params — this is a slide tool).
  2. Register the ``build_slides`` tool + transcription on the realtime session.
  3. Trigger a short opening greeting.

Tool flow (async, mirrors the original request/ack/inject pattern):
  - Voice agent emits ``response.function_call_arguments.done`` for build_slides.
  - We reply with a synchronous ``function_call_output`` ("pending").
  - A background task runs ``SlideAgent.apply(instruction)`` — gpt-5.4 rewrites
    the whole deck and we render + save + push it to the browser.
  - When it resolves we inject a ``[BUILD RESULT] …`` system message and trigger
    another ``response.create`` so the agent confirms the change aloud.

The rendered deck is saved to ``slides/deck.html`` (self-contained) on every
build, and also pushed to the browser as ``ui.deck_update`` for live preview.
"""
from __future__ import annotations

import asyncio
import json
import logging
from pathlib import Path

import httpx
from fastapi import FastAPI, Request, WebSocket, WebSocketDisconnect
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
import websockets

from .auth import ApiKeyAuth
from .client import RealtimeClient, SessionOptions
from .config import load_config
from .deck import Deck, render_deck_editor_html, render_deck_html
from .slide_agent import SlideAgent
from .logging_setup import setup_logging
from .tools import BASE_INSTRUCTIONS, BUILD_SLIDES_TOOL

_ALLOWED_CLIENT_EVENTS = frozenset({
    "input_audio_buffer.append",
    "input_audio_buffer.commit",
    "input_audio_buffer.clear",
    "conversation.item.create",  # typed-command fallback
    "response.create",
    "response.cancel",
    "session.update",
})

log = logging.getLogger("voice_agent.server")

WEB_DIR = Path(__file__).resolve().parent.parent / "web"
SLIDES_DIR = Path(__file__).resolve().parent.parent / "slides"
# Shared source of truth: full deck snapshot (incl. image base64). Both the AI
# agent and the manual /editor read & write this, so edits from one are visible
# to the other.
DECK_JSON = SLIDES_DIR / "deck.json"

config = load_config()
setup_logging(config.log_level)

app = FastAPI(title="Slide Creator Relay")


@app.get("/healthz")
async def healthz() -> dict[str, str]:
    return {"status": "ok", "deployment": config.deployment}


@app.get("/api/config")
async def get_api_config() -> JSONResponse:
    """The frontend hits this on boot. The Python server is configured from env
    vars at startup, so it always reports configured=True. Never return raw keys."""
    return JSONResponse({
        "configured": True,
        "azure_endpoint": config.endpoint,
        "azure_deployment": config.deployment,
        "azure_api_version": config.api_version,
        "slide_deployment": config.slide_deployment,
        "slide_api_version": config.slide_api_version,
        "image_deployment": config.image_deployment,
        "voice": config.voice,
        "reasoning_effort": config.reasoning_effort,
        "azure_api_key_set": bool(config.api_key),
    })


@app.post("/api/config")
async def post_api_config(_request: Request) -> JSONResponse:
    return JSONResponse(
        {"ok": False, "error": "config is sourced from environment; edit .env and restart"},
        status_code=400,
    )


# ——— Manual editor ————————————————————————————————————————————————————————
# The /editor page loads the current deck into a WYSIWYG surface. It reads and
# writes slides/deck.json — the SAME file the AI agent uses — so a manual edit
# is picked up by the next AI build (SlideAgent reloads deck.json before each
# apply) and vice-versa. We never edit manually and via AI at the same instant;
# the file is the hand-off point.


def _load_deck() -> Deck:
    """Load the shared deck.json, or an empty deck if none exists yet."""
    if DECK_JSON.exists():
        try:
            return Deck.from_full_dict(json.loads(DECK_JSON.read_text("utf-8")))
        except Exception:
            log.exception("failed to read %s", DECK_JSON)
    return Deck()


def _save_deck_full(deck: Deck) -> None:
    """Persist both the full snapshot (deck.json) and the presentable export
    (deck.html). Mirrors what SlideAgent does after an AI build."""
    SLIDES_DIR.mkdir(parents=True, exist_ok=True)
    DECK_JSON.write_text(json.dumps(deck.to_full_dict(), ensure_ascii=False), "utf-8")
    (SLIDES_DIR / "deck.html").write_text(render_deck_html(deck), "utf-8")


@app.get("/editor")
async def get_editor() -> FileResponse:
    """Serve the editor chrome (the page that hosts the WYSIWYG iframe)."""
    return FileResponse(WEB_DIR / "editor.html")


@app.get("/api/deck")
async def get_deck() -> JSONResponse:
    """Return the current deck: outline metadata + the WYSIWYG editor document
    the iframe loads via srcdoc."""
    deck = _load_deck()
    return JSONResponse({
        "title": deck.title,
        "slides": [{"id": s.id, "title": s.title} for s in deck.slides],
        "editor_html": render_deck_editor_html(deck),
        "images": [
            {"id": iid, "prompt": img.get("prompt", "")}
            for iid, img in deck.images.items()
        ],
    })


@app.post("/api/deck")
async def post_deck(request: Request) -> JSONResponse:
    """Save edits from the /editor. Body: {title?, slides:[{id,html,title?}]}.
    We merge slide html/titles onto the on-disk deck by id (theme + images are
    preserved untouched), then write deck.json + deck.html."""
    body = await request.json()
    deck = _load_deck()

    new_title = str(body.get("title") or "").strip()
    if new_title:
        deck.title = new_title

    by_id = {s.id: s for s in deck.slides}
    updated = 0
    for entry in body.get("slides") or []:
        sid = str(entry.get("id") or "")
        slide = by_id.get(sid)
        if not slide:
            continue
        if entry.get("html") is not None:
            slide.html = str(entry["html"])
        if entry.get("title") is not None:
            slide.title = str(entry["title"]).strip()
        updated += 1

    try:
        await asyncio.to_thread(_save_deck_full, deck)
    except Exception:
        log.exception("failed to save edited deck")
        return JSONResponse({"ok": False, "error": "save failed"}, status_code=500)
    log.info("editor saved deck (%d/%d slides updated)", updated, len(deck.slides))
    return JSONResponse({"ok": True, "updated": updated, "slides": len(deck.slides)})


@app.post("/api/editor/image")
async def post_editor_image(request: Request) -> JSONResponse:
    """Add an image to the deck and return its id + data URI for live preview.

    Two modes:
      {prompt, size?}  → generate with gpt-image-2 (same path as the AI agent)
      {data_uri}       → store an uploaded image as-is
    The image is registered on the on-disk deck (deck.json) under a fresh id;
    the caller places it on the selected <img> via postMessage, then Save
    persists the slide html referencing it."""
    body = await request.json()
    deck = _load_deck()

    data_uri = str(body.get("data_uri") or "").strip()
    prompt = str(body.get("prompt") or "").strip()

    if not data_uri:
        if not prompt:
            return JSONResponse({"ok": False, "error": "need prompt or data_uri"}, status_code=400)
        size = str(body.get("size") or "1536x1024")
        try:
            data_uri = await _generate_image_data_uri(prompt, size)
        except Exception as exc:
            log.exception("editor image generation failed")
            return JSONResponse({"ok": False, "error": f"{type(exc).__name__}: {exc}"}, status_code=502)

    iid = deck.new_image_id()
    deck.images[iid] = {"data_uri": data_uri, "prompt": prompt or "uploaded image"}
    try:
        await asyncio.to_thread(
            lambda: DECK_JSON.write_text(
                json.dumps(deck.to_full_dict(), ensure_ascii=False), "utf-8"
            )
        )
    except Exception:
        log.exception("failed to persist image to deck.json")
        return JSONResponse({"ok": False, "error": "save failed"}, status_code=500)
    return JSONResponse({"ok": True, "image_id": iid, "data_uri": data_uri})


async def _generate_image_data_uri(prompt: str, size: str) -> str:
    """Call gpt-image-2 and return a data URI. Mirrors SlideAgent._generate_image."""
    url = config.image_gen_url
    headers = {"api-key": config.api_key or "", "Content-Type": "application/json"}
    body = {"prompt": prompt, "n": 1, "size": size}
    async with httpx.AsyncClient(timeout=180.0) as http:
        resp = await http.post(url, headers=headers, json=body)
    if resp.status_code != 200:
        raise RuntimeError(f"image http {resp.status_code}: {resp.text[:200]}")
    items = resp.json().get("data") or []
    b64 = items[0].get("b64_json") if items else None
    if not b64:
        raise RuntimeError("no image returned")
    return f"data:image/png;base64,{b64}"


class SessionState:
    """Per-connection state. Serializes response.create calls and owns the
    slide-building worker."""

    def __init__(self) -> None:
        self.response_in_flight = False
        self.lock = asyncio.Lock()
        self.slides: SlideAgent | None = None


@app.websocket("/ws")
async def ws_relay(browser: WebSocket) -> None:
    await browser.accept()
    log.info("browser connected")

    state = SessionState()
    session = SessionOptions(
        voice=config.voice,
        instructions=BASE_INSTRUCTIONS,
        reasoning_effort=config.reasoning_effort,
    )
    auth = ApiKeyAuth(config.api_key or "")
    client = RealtimeClient(config, auth, session)

    try:
        await client.__aenter__()
    except Exception as exc:
        log.exception("failed to connect to Azure Realtime")
        await _safe_send_json(browser, {"type": "error", "error": {
            "code": "upstream_connect_failed", "type": type(exc).__name__,
        }})
        await browser.close()
        return

    # The slide worker pushes deck updates + status straight to the browser,
    # and persists each render to slides/deck.html.
    state.slides = SlideAgent(
        cfg=config,
        emit_deck=lambda payload: _safe_send_json(
            browser, {"type": "ui.deck_update", **payload}
        ),
        emit_status=lambda payload: _safe_send_json(
            browser, {"type": "ui.slide_status", **payload}
        ),
        on_saved=_save_deck,
        deck_path=DECK_JSON,
    )

    # Register tool + transcription before kicking off the opening turn.
    await client._send({  # noqa: SLF001
        "type": "session.update",
        "session": {
            "tools": [BUILD_SLIDES_TOOL],
            "tool_choice": "auto",
            "input_audio_transcription": {"model": "whisper-1"},
        },
    })

    # Opening greeting — no external data to wait for, so fire right away.
    asyncio.create_task(_open_greeting(client, state))

    pending_tools: set[asyncio.Task] = set()
    browser_to_azure = asyncio.create_task(_pump_browser_to_azure(browser, client, state))
    azure_to_browser = asyncio.create_task(
        _pump_azure_to_browser(client, browser, pending_tools, state)
    )

    done, pending = await asyncio.wait(
        {browser_to_azure, azure_to_browser},
        return_when=asyncio.FIRST_COMPLETED,
    )
    for task in pending:
        task.cancel()
    for task in pending_tools:
        task.cancel()
    for task in done:
        exc = task.exception()
        if exc and not isinstance(exc, (WebSocketDisconnect, websockets.ConnectionClosed)):
            log.exception("relay task error", exc_info=exc)

    await client.close()
    await _safe_close(browser)
    log.info("browser disconnected")


def _save_deck(html: str) -> "asyncio.Future":
    """Persist the rendered deck to slides/deck.html (best-effort, off-thread)."""
    async def _write() -> None:
        try:
            SLIDES_DIR.mkdir(parents=True, exist_ok=True)
            await asyncio.to_thread(
                (SLIDES_DIR / "deck.html").write_text, html, "utf-8"
            )
            log.info("saved deck to %s", SLIDES_DIR / "deck.html")
        except Exception:
            log.exception("failed to save deck.html")
    return asyncio.ensure_future(_write())


async def _request_response(
    client: RealtimeClient,
    state: SessionState,
    instructions: str | None = None,
) -> bool:
    """Send response.create iff nothing is already in flight."""
    async with state.lock:
        if state.response_in_flight:
            log.info("response.create skipped — one already in flight")
            return False
        state.response_in_flight = True
    payload: dict = {"type": "response.create"}
    if instructions:
        payload["response"] = {"instructions": instructions}
    await client._send(payload)  # noqa: SLF001
    return True


async def _open_greeting(client: RealtimeClient, state: SessionState) -> None:
    try:
        await _request_response(
            client, state,
            instructions=(
                "Greet the user in one short, upbeat sentence and invite them to "
                "describe the slides they want — for example, how many slides and "
                "what each one should cover. Keep it under two sentences."
            ),
        )
    except (AssertionError, websockets.ConnectionClosed) as exc:
        log.info("open_greeting: upstream gone (%s)", type(exc).__name__)


async def _pump_browser_to_azure(
    browser: WebSocket, client: RealtimeClient, state: SessionState
) -> None:
    while True:
        try:
            raw = await browser.receive_text()
        except WebSocketDisconnect:
            return
        try:
            event = json.loads(raw)
        except json.JSONDecodeError:
            log.warning("dropped non-json frame from browser bytes=%d", len(raw))
            continue
        etype = event.get("type")
        if etype not in _ALLOWED_CLIENT_EVENTS:
            log.warning("dropped disallowed client event type=%s", etype)
            continue

        if etype == "response.create":
            await _request_response(
                client, state,
                instructions=(event.get("response") or {}).get("instructions"),
            )
            continue

        if etype == "response.cancel":
            await client._send(event)  # noqa: SLF001
            async with state.lock:
                state.response_in_flight = False
            continue

        await client._send(event)  # noqa: SLF001


async def _pump_azure_to_browser(
    client: RealtimeClient,
    browser: WebSocket,
    pending_tools: set[asyncio.Task],
    state: SessionState,
) -> None:
    async for event in client.events():
        etype = event.get("type", "")

        if etype == "response.created":
            async with state.lock:
                state.response_in_flight = True

        elif etype in ("response.done", "response.cancelled"):
            async with state.lock:
                state.response_in_flight = False

        elif etype == "response.function_call_arguments.done":
            call_id = event.get("call_id")
            name = event.get("name") or event.get("function", {}).get("name")
            args_str = event.get("arguments") or "{}"
            try:
                args = json.loads(args_str)
            except json.JSONDecodeError:
                args = {}
            task = asyncio.create_task(
                _handle_tool_call(client, browser, state, call_id, name, args)
            )
            pending_tools.add(task)
            task.add_done_callback(pending_tools.discard)

        await _safe_send_json(browser, event)


async def _handle_tool_call(
    client: RealtimeClient,
    browser: WebSocket,
    state: SessionState,
    call_id: str | None,
    name: str | None,
    args: dict,
) -> None:
    if not call_id or name != "build_slides":
        return
    instruction = str(args.get("instruction", "")).strip()
    log.info("tool call build_slides call_id=%s len=%d", call_id, len(instruction))

    await _safe_send_json(browser, {
        "type": "ui.build_pending", "call_id": call_id,
        "instruction": instruction[:140],
    })

    # 1) Synchronous ack so the agent can keep talking naturally.
    await client._send({  # noqa: SLF001
        "type": "conversation.item.create",
        "item": {
            "type": "function_call_output",
            "call_id": call_id,
            "output": json.dumps({
                "status": "pending",
                "note": "Building the deck now; result arrives as [BUILD RESULT].",
            }),
        },
    })

    # 2) Actually build (slow — gpt-5.4 rewrites the deck).
    summary = "Slide build unavailable."
    if state.slides is not None:
        summary = await state.slides.apply(instruction)

    # 3) Inject the result and prompt a confirmation turn.
    await _safe_send_json(browser, {
        "type": "ui.build_resolved", "call_id": call_id, "summary": summary,
    })
    await client._send({  # noqa: SLF001
        "type": "conversation.item.create",
        "item": {
            "type": "message",
            "role": "system",
            "content": [{"type": "input_text", "text": f"[BUILD RESULT] {summary}"}],
        },
    })
    await _request_response(
        client, state,
        instructions=(
            "Confirm the slide change in one short sentence and invite the next "
            "edit. Do not read any markup. Do not repeat the [BUILD RESULT] tag."
        ),
    )


async def _safe_send_json(ws: WebSocket, payload: dict) -> None:
    try:
        await ws.send_text(json.dumps(payload))
    except Exception:
        pass


async def _safe_close(ws: WebSocket) -> None:
    try:
        await ws.close()
    except Exception:
        pass


# Serve saved decks so the "Open ↗" link (and direct presenting) works. Created
# on demand so the mount has a directory to point at even before the first build.
SLIDES_DIR.mkdir(parents=True, exist_ok=True)
app.mount("/slides", StaticFiles(directory=SLIDES_DIR), name="slides")

# Static frontend. Mounted LAST so /healthz, /api/*, /ws, /slides win matching.
app.mount("/", StaticFiles(directory=WEB_DIR, html=True), name="web")
