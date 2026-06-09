"""Slide-building agent (Azure chat-completions, gpt-5.4).

The realtime *voice* agent hands this worker a natural-language instruction
("make five slides about AI in space", "slide 5 needs a chart of launch costs",
"make it all cohesive"). This worker holds the authoritative deck and applies
the instruction by calling **granular tools** — so a one-slide tweak edits only
that slide instead of regenerating the whole deck.

Operations are id-based, not index-based: every slide carries a stable id (shown
in the deck JSON the model sees). Edit/delete/move reference that id, so a batch
of ops in one turn can't be thrown off by positions shifting underneath them.

Tools:
  * write_deck   — replace the whole deck (use for "start over" / first build /
                   a restructure touching most slides).
  * add_slide    — insert one slide (optionally after a given id).
  * edit_slide   — change one slide's title and/or html.
  * delete_slide — remove one slide.
  * move_slide   — reorder one slide to a new position.
  * set_theme    — replace theme_css (restyles the whole deck — one stylesheet).
  * set_title    — rename the deck.

The model may call several tools in one turn (e.g. edit two slides + set_theme).
We apply them in order to a working copy, then render + save + push once.

Look-and-feel lives entirely in ``theme_css``; slides are semantic HTML written
against a small fixed class vocabulary. That separation is what makes a restyle
touch one stylesheet (set_theme) instead of N slides.
"""
from __future__ import annotations

import asyncio
import copy
import json
import logging
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Awaitable, Callable

import httpx

from .config import RealtimeConfig
from .deck import Deck, Slide, render_deck_html

log = logging.getLogger("voice_agent.slide_agent")

REQUEST_TIMEOUT_S = 180.0
MAX_TOOL_ROUNDS = 4          # chat rounds per instruction (each may batch calls)
MAX_OPS_PER_INSTRUCTION = 40  # backstop against a runaway batch


# Shared design guidance referenced by the system prompt. Kept in one place so
# the vocabulary stays consistent across write_deck and edit/add operations.
_VOCAB_AND_STYLE = """CANVAS & LAYOUT — every slide renders on a FIXED 16:9 canvas (think 1280×720). The deck mechanics do NOT scroll: content is vertically centered inside the slide, and a slide that overflows gets clipped or auto-shrunk. So you must DESIGN EACH SLIDE TO FIT. Concretely:
- The usable content area is roughly 80% of the width and 86% of the height (the rest is slide padding). Plan for ~1040×620 of room.
- Budget vertical space. A slide that stacks, top-to-bottom: kicker + title + lead + a 5-item bullet list + a large image WILL NOT FIT. Pick fewer elements per slide.
- IMAGE + TEXT: never stack a big image ABOVE/BELOW a block of text — they won't both fit. Put them SIDE BY SIDE using `columns`: image in one `col`, text in the other. A full-bleed/hero image slide should have only a short overlaid title, not paragraphs.
- Keep it sparse: a slide is ~1 headline + at most ~5 short bullets OR a lead paragraph OR one image+caption — not all of them. If you have more, split into another slide.
- Bullets: aim for 3–5, each one line. Lead paragraphs: 1–3 sentences.

SLIDE HTML — write semantic HTML against this FIXED class vocabulary so the one shared theme styles every slide uniformly. NEVER put inline `style=` attributes or `<style>` tags in slide html — all styling lives in theme_css.
- `<div class="kicker">…</div>`        small eyebrow label above the title
- `<h1 class="slide-title">…</h1>`     the slide headline
- `<h2 class="slide-subtitle">…</h2>`  secondary headline
- `<p class="slide-lead">…</p>`        a lead paragraph / intro sentence
- `<ul class="bullets"><li>…</li></ul>` bullet list
- `<div class="columns"><div class="col">…</div><div class="col">…</div></div>` multi-column layout
- `<div class="big-number">42%</div>` + `<div class="label">…</div>`  stat callouts
- `<div class="card">…</div>`          a boxed panel (nest other elements inside)
- `<blockquote class="quote">…</blockquote>`  pull quote
- `<div class="footer">…</div>`        per-slide footer line
You may nest these freely (e.g. cards inside columns). Use only these classes plus the structural tags shown.

THEME_CSS — one stylesheet for the whole deck. Style the vocabulary classes above plus `body`, `.slide`, and `.slide-inner`. Make it genuinely designed: a real colour system, a type scale, generous spacing, a personality that fits the topic. You may set `body{background:…}`, pick fonts from the system stack, etc. No external assets, no @import, no web fonts — must work fully offline.

IMAGES — you can generate real images with the `generate_image` tool. It returns an image `id`. To show that image on a slide, put `<img class="slide-image" data-img="THE_ID" alt="...">` in the slide html (no `src` — it is filled in automatically). Reuse an existing image by its id from the deck's `images` list instead of regenerating. Workflow: call generate_image, read the returned id, THEN edit_slide/add_slide/write_deck to place `data-img="<id>"`. Size the image in theme_css via `.slide-image` (e.g. constrain with max-height). LAYOUT WITH TEXT: if a slide has both an image and bullets/paragraphs, wrap them in `columns` (image in one `col`, text in the other) so they sit side by side and both fit — do NOT stack a large image over a text block. Match the image's aspect to its placement: landscape (1536x1024) for hero/wide, portrait (1024x1536) for a side column next to text. Only generate images when they add value — don't put one on every slide."""


SYSTEM_PROMPT = f"""You are an expert slide-deck designer. You apply a spoken instruction to an HTML slide deck by calling tools. You are given the CURRENT deck as JSON, including each slide's stable `id`.

CHOOSE THE SMALLEST EDIT THAT DOES THE JOB:
- Tweaking, rewriting, or fixing ONE slide → `edit_slide` on that slide's id. Do NOT touch other slides.
- Adding a slide → `add_slide` (use `after_id` to position it).
- Removing a slide → `delete_slide`. Reordering → `move_slide`.
- Restyling / "make it cohesive" / "match the colours" → `set_theme` with one unified stylesheet. This restyles every slide at once; you usually do NOT need to edit slide html for a restyle, because slides already use the shared class vocabulary.
- Renaming the deck → `set_title`.
- Adding a picture/illustration/photo to a slide → `generate_image` first (get its id), then place it with `data-img`.
- Building a brand-new deck, or a restructure that changes most slides → `write_deck` with the complete deck.

You MAY call several tools in one turn (e.g. edit two slides, or edit one slide AND set_theme). Reference slides by `id`, never by guessed position. Only change what the instruction asks for; leave everything else untouched.

{_VOCAB_AND_STYLE}

CONTENT RULES
- Respect explicit slide counts and per-slide topics. A 5-slide request → 5 slides.
- Write real, substantive content — actual bullets and sentences, not placeholders.
- A title slide is usually kicker + slide-title + slide-lead. Keep slides uncluttered.

Do not emit any assistant text — only tool calls. After your tool calls are applied you'll get the result and should stop."""


# ——— Tool schemas ———
_SLIDE_PROPS = {
    "title": {"type": "string", "description": "Plain-text slide title for the outline."},
    "html": {
        "type": "string",
        "description": ("Inner HTML using the fixed class vocabulary. "
                        "No inline styles or <style> tags."),
    },
}

WRITE_DECK_TOOL = {
    "type": "function",
    "function": {
        "name": "write_deck",
        "description": (
            "Replace the ENTIRE deck. Use only for a brand-new deck or a "
            "restructure that changes most slides. Include every slide that "
            "should exist after, in order."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "Short deck title."},
                "theme_css": {"type": "string", "description": "One stylesheet for the whole deck."},
                "slides": {
                    "type": "array",
                    "items": {"type": "object", "properties": _SLIDE_PROPS, "required": ["title", "html"]},
                },
            },
            "required": ["title", "theme_css", "slides"],
        },
    },
}

ADD_SLIDE_TOOL = {
    "type": "function",
    "function": {
        "name": "add_slide",
        "description": "Insert ONE new slide. By default appends to the end.",
        "parameters": {
            "type": "object",
            "properties": {
                **_SLIDE_PROPS,
                "after_id": {
                    "type": "string",
                    "description": ("Insert the new slide immediately after the slide "
                                    "with this id. Omit to append at the end."),
                },
            },
            "required": ["title", "html"],
        },
    },
}

EDIT_SLIDE_TOOL = {
    "type": "function",
    "function": {
        "name": "edit_slide",
        "description": ("Replace the title and/or html of ONE existing slide, "
                        "identified by id. Other slides are untouched."),
        "parameters": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Id of the slide to edit."},
                "title": {"type": "string", "description": "New title (omit to keep current)."},
                "html": {"type": "string", "description": "New inner html (omit to keep current)."},
            },
            "required": ["id"],
        },
    },
}

DELETE_SLIDE_TOOL = {
    "type": "function",
    "function": {
        "name": "delete_slide",
        "description": "Remove ONE slide by id.",
        "parameters": {
            "type": "object",
            "properties": {"id": {"type": "string", "description": "Id of the slide to delete."}},
            "required": ["id"],
        },
    },
}

MOVE_SLIDE_TOOL = {
    "type": "function",
    "function": {
        "name": "move_slide",
        "description": "Reorder ONE slide to a new zero-based position.",
        "parameters": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Id of the slide to move."},
                "to_index": {"type": "integer", "description": "New 0-based position."},
            },
            "required": ["id", "to_index"],
        },
    },
}

SET_THEME_TOOL = {
    "type": "function",
    "function": {
        "name": "set_theme",
        "description": ("Replace theme_css for the whole deck. This is how you "
                        "restyle / make a deck cohesive — one stylesheet restyles "
                        "every slide. No need to edit slide html."),
        "parameters": {
            "type": "object",
            "properties": {"theme_css": {"type": "string", "description": "The new full stylesheet."}},
            "required": ["theme_css"],
        },
    },
}

SET_TITLE_TOOL = {
    "type": "function",
    "function": {
        "name": "set_title",
        "description": "Rename the deck.",
        "parameters": {
            "type": "object",
            "properties": {"title": {"type": "string", "description": "New deck title."}},
            "required": ["title"],
        },
    },
}

GENERATE_IMAGE_TOOL = {
    "type": "function",
    "function": {
        "name": "generate_image",
        "description": (
            "Generate a real image from a text prompt (gpt-image-2) and return "
            "its image id. Place it on a slide with "
            "`<img class=\"slide-image\" data-img=\"<id>\">`. Multiple "
            "generate_image calls in one turn run concurrently. Write a vivid, "
            "specific prompt (subject, style, composition, mood). Reuse an "
            "existing image (from the deck's images list) instead of "
            "regenerating the same thing."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Detailed description of the image to generate.",
                },
                "size": {
                    "type": "string",
                    "enum": ["1024x1024", "1536x1024", "1024x1536"],
                    "description": ("Aspect: square, landscape (good for hero "
                                    "visuals), or portrait. Default landscape."),
                },
            },
            "required": ["prompt"],
        },
    },
}

TOOLS = [
    WRITE_DECK_TOOL, ADD_SLIDE_TOOL, EDIT_SLIDE_TOOL, DELETE_SLIDE_TOOL,
    MOVE_SLIDE_TOOL, SET_THEME_TOOL, SET_TITLE_TOOL, GENERATE_IMAGE_TOOL,
]


DeckEmitter = Callable[[dict], Awaitable[None]]
StatusEmitter = Callable[[dict], Awaitable[None]]


@dataclass
class SlideAgent:
    cfg: RealtimeConfig
    emit_deck: DeckEmitter        # push {deck dict, html} to the browser
    emit_status: StatusEmitter    # push {state, note} to the browser
    on_saved: Callable[[str], Awaitable[None]] | None = None  # called w/ html to persist
    # Path to the shared deck.json source of truth. The agent reloads it before
    # every edit so manual edits made in the /editor (which write this file) are
    # visible to the model — and writes it back after every edit. None disables
    # disk sync (e.g. in tests).
    deck_path: Path | None = None

    deck: Deck = field(default_factory=Deck)
    run_count: int = 0
    _lock: asyncio.Lock = field(default_factory=asyncio.Lock)

    def __post_init__(self) -> None:
        # Adopt any deck already on disk (from a prior session or manual edit).
        self._load_from_disk()

    def _load_from_disk(self) -> bool:
        """Replace self.deck with the on-disk deck.json if present and valid.
        Returns True if a deck was loaded."""
        if not self.deck_path or not self.deck_path.exists():
            return False
        try:
            data = json.loads(self.deck_path.read_text("utf-8"))
            self.deck = Deck.from_full_dict(data)
            log.info("loaded deck from %s (%d slides)", self.deck_path, len(self.deck.slides))
            return True
        except Exception:
            log.exception("failed to load %s — keeping in-memory deck", self.deck_path)
            return False

    def _persist_to_disk(self, deck: Deck) -> None:
        """Write the full deck snapshot to deck.json (the shared source of truth)."""
        if not self.deck_path:
            return
        try:
            self.deck_path.parent.mkdir(parents=True, exist_ok=True)
            self.deck_path.write_text(
                json.dumps(deck.to_full_dict(), ensure_ascii=False), "utf-8"
            )
        except Exception:
            log.exception("failed to persist %s", self.deck_path)

    async def apply(self, instruction: str) -> str:
        """Apply one natural-language instruction via granular tool calls.
        Returns a short status line the voice agent can paraphrase."""
        instruction = (instruction or "").strip()
        if not instruction:
            return "No instruction given."
        # Serialize: never let two edits race on self.deck.
        async with self._lock:
            self.run_count += 1
            run_id = self.run_count
            await self._safe_status("building", note=instruction[:80])

            # Pick up any manual edits made in the /editor since the last build,
            # so the model edits the deck the user is actually looking at.
            self._load_from_disk()

            # Work on a copy so a mid-way failure can't leave a half-applied deck.
            working = copy.deepcopy(self.deck)
            try:
                applied = await self._run_tool_loop(working, instruction)
            except Exception as exc:
                log.exception("slide edit failed")
                await self._safe_status("error", note=f"{type(exc).__name__}")
                return f"Slide edit failed: {type(exc).__name__}."

            if not applied:
                await self._safe_status("ready", note="no change")
                return "I didn't change anything — could you rephrase what you want?"

            working.ensure_ids()
            self.deck = working
            # Persist the shared source of truth first so the /editor and a
            # restart both see this build.
            self._persist_to_disk(working)
            html = render_deck_html(working)
            try:
                await self.emit_deck({"deck": working.to_dict(), "html": html})
            except Exception:
                log.exception("emit_deck failed")
            if self.on_saved is not None:
                try:
                    await self.on_saved(html)
                except Exception:
                    log.exception("on_saved failed")

            summary = self._summarize(applied, working)
            await self._safe_status("ready", note=f"run #{run_id} · {summary}")
            return summary

    # ——— tool loop ———
    async def _run_tool_loop(self, working: Deck, instruction: str) -> list[str]:
        """Drive the model through up to MAX_TOOL_ROUNDS, applying each tool call
        to `working`. Returns a list of human-readable op descriptions applied."""
        messages: list[dict[str, Any]] = [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": (
                "[CURRENT DECK — JSON, slides carry stable ids]\n"
                f"{json.dumps(working.to_dict(), ensure_ascii=False)}\n\n"
                f"[INSTRUCTION]\n{instruction}\n\n"
                "Apply it with the smallest set of tool calls. Reference slides by id."
            )},
        ]
        applied: list[str] = []

        async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT_S) as http:
            for _round in range(MAX_TOOL_ROUNDS):
                msg = await self._chat(http, messages)
                messages.append(msg)
                tool_calls = msg.get("tool_calls") or []
                if not tool_calls:
                    break

                # Parse every call in the batch up front.
                parsed: list[tuple[dict, str, dict]] = []
                for call in tool_calls:
                    name = (call.get("function") or {}).get("name", "")
                    args_str = (call.get("function") or {}).get("arguments") or "{}"
                    try:
                        args = json.loads(args_str)
                    except json.JSONDecodeError:
                        args = {}
                    parsed.append((call, name, args))

                # Image generation is slow — fire every generate_image in this
                # batch concurrently, then store the results before applying
                # ops in call order. (Mutation ops in the same batch normally
                # come in a *later* round once the model has the image id, but
                # this also handles a batch that mixes them.)
                img_calls = [(c, a) for c, n, a in parsed if n == "generate_image"]
                img_results: dict[int, dict] = {}
                if img_calls:
                    await self._safe_status("imaging", note=f"{len(img_calls)} image(s)")
                    coros = [self._generate_image(http, working, a) for _c, a in img_calls]
                    gathered = await asyncio.gather(*coros, return_exceptions=True)
                    for (call, _a), res in zip(img_calls, gathered):
                        if isinstance(res, BaseException):
                            res = {"ok": False, "error": f"{type(res).__name__}: {res}"}
                        img_results[id(call)] = res

                for call, name, args in parsed:
                    if len(applied) >= MAX_OPS_PER_INSTRUCTION:
                        result = {"ok": False, "error": "op limit reached"}
                    elif name == "generate_image":
                        result = img_results.get(id(call), {"ok": False, "error": "image task lost"})
                    else:
                        result = self._dispatch(working, name, args)
                    if result.get("ok") and result.get("desc"):
                        applied.append(result["desc"])
                    messages.append({
                        "role": "tool",
                        "tool_call_id": call.get("id", ""),
                        "content": json.dumps(result),
                    })
        return applied

    # ——— image generation (async, hits gpt-image-2) ———
    async def _generate_image(
        self, http: httpx.AsyncClient, deck: Deck, args: dict[str, Any]
    ) -> dict[str, Any]:
        prompt = str(args.get("prompt", "")).strip()
        if not prompt:
            return {"ok": False, "error": "empty prompt"}
        size = str(args.get("size", "")).strip() or "1536x1024"
        url = self.cfg.image_gen_url
        headers = {"api-key": self.cfg.api_key or "", "Content-Type": "application/json"}
        body = {"prompt": prompt, "n": 1, "size": size}
        try:
            resp = await http.post(url, headers=headers, json=body, timeout=REQUEST_TIMEOUT_S)
        except Exception as exc:
            log.exception("image gen request failed")
            return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        if resp.status_code != 200:
            return {"ok": False, "error": f"image http {resp.status_code}: {resp.text[:200]}"}
        data = resp.json()
        items = data.get("data") or []
        b64 = items[0].get("b64_json") if items else None
        if not b64:
            return {"ok": False, "error": "no image returned"}
        iid = deck.new_image_id()
        deck.images[iid] = {
            "data_uri": f"data:image/png;base64,{b64}",
            "prompt": prompt,
        }
        log.info("generated image id=%s size=%s bytes=%d", iid, size, len(b64))
        return {
            "ok": True,
            "image_id": iid,
            "desc": f"generated an image",
            "note": (f'Use it on a slide with <img class="slide-image" '
                     f'data-img="{iid}">.'),
        }

    # ——— op dispatch (pure, operates on the working deck) ———
    def _dispatch(self, deck: Deck, name: str, args: dict[str, Any]) -> dict[str, Any]:
        log.info("slide op name=%s args_keys=%s", name, sorted(args.keys()))
        try:
            if name == "write_deck":
                return self._op_write_deck(deck, args)
            if name == "add_slide":
                return self._op_add_slide(deck, args)
            if name == "edit_slide":
                return self._op_edit_slide(deck, args)
            if name == "delete_slide":
                return self._op_delete_slide(deck, args)
            if name == "move_slide":
                return self._op_move_slide(deck, args)
            if name == "set_theme":
                return self._op_set_theme(deck, args)
            if name == "set_title":
                return self._op_set_title(deck, args)
        except Exception as exc:  # never let one bad op kill the turn
            log.exception("op %s failed", name)
            return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        return {"ok": False, "error": f"unknown tool: {name}"}

    def _op_write_deck(self, deck: Deck, args: dict) -> dict:
        deck.theme_css = str(args.get("theme_css", ""))
        deck.slides = []
        for s in args.get("slides") or []:
            if not isinstance(s, dict):
                continue
            deck.slides.append(Slide(
                title=str(s.get("title", "")).strip(),
                html=str(s.get("html", "")),
                id=deck.new_id(),
            ))
        # Prefer the model's title; if it omitted one, fall back to the first
        # slide's title before the generic placeholder — "Untitled Deck" in the
        # tab/outline looks broken when slide 1 clearly names the topic.
        title = str(args.get("title", "")).strip()
        if not title and deck.slides:
            title = deck.slides[0].title
        deck.title = title or "Untitled Deck"
        return {"ok": True, "desc": f"rebuilt deck ({len(deck.slides)} slides)",
                "slides": [{"id": s.id, "title": s.title} for s in deck.slides]}

    def _op_add_slide(self, deck: Deck, args: dict) -> dict:
        slide = Slide(
            title=str(args.get("title", "")).strip(),
            html=str(args.get("html", "")),
            id=deck.new_id(),
        )
        after_id = str(args.get("after_id", "")).strip()
        if after_id:
            idx = deck.index_of(after_id)
            if idx < 0:
                deck.slides.append(slide)
            else:
                deck.slides.insert(idx + 1, slide)
        else:
            deck.slides.append(slide)
        # If this is the first content on a still-unnamed deck, adopt the slide's
        # title so the tab/outline don't show a stray "Untitled Deck".
        if deck.title in ("", "Untitled Deck") and slide.title:
            deck.title = slide.title
        return {"ok": True, "desc": f"added slide “{slide.title}”", "id": slide.id}

    def _op_edit_slide(self, deck: Deck, args: dict) -> dict:
        sid = str(args.get("id", "")).strip()
        idx = deck.index_of(sid)
        if idx < 0:
            return {"ok": False, "error": f"no slide with id {sid!r}"}
        s = deck.slides[idx]
        changed = []
        if "title" in args and args["title"] is not None:
            s.title = str(args["title"]).strip()
            changed.append("title")
        if "html" in args and args["html"] is not None:
            s.html = str(args["html"])
            changed.append("content")
        if not changed:
            return {"ok": False, "error": "nothing to change (no title or html)"}
        return {"ok": True, "desc": f"edited slide {idx + 1} ({'/'.join(changed)})"}

    def _op_delete_slide(self, deck: Deck, args: dict) -> dict:
        sid = str(args.get("id", "")).strip()
        idx = deck.index_of(sid)
        if idx < 0:
            return {"ok": False, "error": f"no slide with id {sid!r}"}
        s = deck.slides.pop(idx)
        return {"ok": True, "desc": f"deleted slide “{s.title}”"}

    def _op_move_slide(self, deck: Deck, args: dict) -> dict:
        sid = str(args.get("id", "")).strip()
        idx = deck.index_of(sid)
        if idx < 0:
            return {"ok": False, "error": f"no slide with id {sid!r}"}
        to = int(args.get("to_index", idx))
        to = max(0, min(len(deck.slides) - 1, to))
        s = deck.slides.pop(idx)
        deck.slides.insert(to, s)
        return {"ok": True, "desc": f"moved slide to position {to + 1}"}

    def _op_set_theme(self, deck: Deck, args: dict) -> dict:
        theme = str(args.get("theme_css", ""))
        if not theme.strip():
            return {"ok": False, "error": "empty theme_css"}
        deck.theme_css = theme
        return {"ok": True, "desc": "restyled the deck"}

    def _op_set_title(self, deck: Deck, args: dict) -> dict:
        title = str(args.get("title", "")).strip()
        if not title:
            return {"ok": False, "error": "empty title"}
        deck.title = title
        return {"ok": True, "desc": f"renamed deck to “{title}”"}

    # ——— LLM call ———
    async def _chat(self, http: httpx.AsyncClient, messages: list[dict]) -> dict[str, Any]:
        body = {
            "messages": messages,
            "tools": TOOLS,
            "tool_choice": "auto",
            "temperature": 0.4,
        }
        url = self.cfg.dashboard_chat_url  # gpt-5.4 chat-completions endpoint
        headers = {"api-key": self.cfg.api_key or "", "Content-Type": "application/json"}
        resp = await http.post(url, headers=headers, json=body)
        if resp.status_code != 200:
            raise RuntimeError(f"slide chat http {resp.status_code}: {resp.text[:400]}")
        data = resp.json()
        msg = (data.get("choices") or [{}])[0].get("message") or {}
        return {
            "role": msg.get("role", "assistant"),
            "content": msg.get("content"),
            "tool_calls": msg.get("tool_calls") or [],
        }

    # ——— helpers ———
    def _summarize(self, applied: list[str], deck: Deck) -> str:
        n = len(deck.slides)
        count = f"{n} slide{'s' if n != 1 else ''}"
        if len(applied) == 1:
            return f"{applied[0].capitalize()} — {count} in “{deck.title}”."
        return f"Applied {len(applied)} changes — {count} in “{deck.title}”."

    async def _safe_status(self, state: str, *, note: str = "") -> None:
        try:
            await self.emit_status({"state": state, "note": note, "run_count": self.run_count})
        except Exception:
            log.debug("emit_status failed", exc_info=True)


__all__ = ["SlideAgent"]
