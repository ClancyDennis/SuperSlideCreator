"""Azure OpenAI Realtime WSS client.

Connects to the Realtime endpoint, sends a session.update, and exposes
async send/receive primitives. Handles reconnect with exponential backoff
and surfaces errors to the caller.
"""
from __future__ import annotations

import asyncio
import base64
import json
import logging
import random
from dataclasses import dataclass
from typing import AsyncIterator, Awaitable, Callable

import websockets
from websockets.asyncio.client import ClientConnection

from .auth import AuthStrategy
from .config import RealtimeConfig

log = logging.getLogger("voice_agent.client")

# Backoff bounds for reconnect. Hospital prod: avoid hammering the endpoint.
_BACKOFF_MIN_S = 1.0
_BACKOFF_MAX_S = 30.0


@dataclass
class SessionOptions:
    voice: str
    instructions: str
    input_audio_format: str = "pcm16"
    output_audio_format: str = "pcm16"
    # None = client-controlled turns. We use PTT in the browser, so no VAD.
    turn_detection_type: str | None = None
    modalities: tuple[str, ...] = ("audio", "text")
    # Realtime-2 only. Empty string omits the field so v1 deployments don't reject it.
    reasoning_effort: str = ""

    def to_session_update(self) -> dict:
        session: dict = {
            "modalities": list(self.modalities),
            "voice": self.voice,
            "instructions": self.instructions,
            "input_audio_format": self.input_audio_format,
            "output_audio_format": self.output_audio_format,
            "turn_detection": (
                None if self.turn_detection_type is None
                else {"type": self.turn_detection_type}
            ),
        }
        if self.reasoning_effort:
            session["reasoning"] = {"effort": self.reasoning_effort}
        return {"type": "session.update", "session": session}


EventHandler = Callable[[dict], Awaitable[None]]


class RealtimeClient:
    def __init__(
        self,
        config: RealtimeConfig,
        auth: AuthStrategy,
        session: SessionOptions,
    ) -> None:
        self._config = config
        self._auth = auth
        self._session = session
        self._ws: ClientConnection | None = None
        self._send_lock = asyncio.Lock()

    async def __aenter__(self) -> "RealtimeClient":
        await self._connect()
        return self

    async def __aexit__(self, *exc) -> None:
        await self.close()

    async def _connect(self) -> None:
        url = self._config.ws_url
        headers = self._auth.headers()
        log.info("connecting to realtime endpoint host=%s deployment=%s",
                 url.split("/")[2], self._config.deployment)
        # websockets>=12 uses `additional_headers` in asyncio.client
        self._ws = await websockets.connect(
            url,
            additional_headers=headers,
            max_size=16 * 1024 * 1024,
            ping_interval=20,
            ping_timeout=20,
            close_timeout=5,
        )
        await self._send(self._session.to_session_update())
        log.info("session.update sent")

    async def close(self) -> None:
        if self._ws is not None:
            await self._ws.close()
            self._ws = None

    async def _send(self, event: dict) -> None:
        assert self._ws is not None, "not connected"
        payload = json.dumps(event)
        async with self._send_lock:
            await self._ws.send(payload)
        log.debug("sent event type=%s bytes=%d", event.get("type"), len(payload))

    async def send_audio_chunk(self, pcm16_bytes: bytes) -> None:
        """Append a chunk of 16-bit PCM mono audio (24kHz) to the input buffer."""
        await self._send({
            "type": "input_audio_buffer.append",
            "audio": base64.b64encode(pcm16_bytes).decode("ascii"),
        })

    async def commit_audio(self) -> None:
        await self._send({"type": "input_audio_buffer.commit"})

    async def request_response(self) -> None:
        await self._send({"type": "response.create"})

    async def events(self) -> AsyncIterator[dict]:
        assert self._ws is not None, "not connected"
        async for raw in self._ws:
            try:
                event = json.loads(raw)
            except json.JSONDecodeError:
                log.warning("non-json frame received bytes=%d", len(raw))
                continue
            yield event


async def run_with_reconnect(
    config: RealtimeConfig,
    auth: AuthStrategy,
    session: SessionOptions,
    on_event: EventHandler,
    on_connected: Callable[[RealtimeClient], Awaitable[None]] | None = None,
    shutdown: asyncio.Event | None = None,
) -> None:
    """Run the client, reconnecting on transient failures until shutdown is set."""
    backoff = _BACKOFF_MIN_S
    shutdown = shutdown or asyncio.Event()

    while not shutdown.is_set():
        try:
            async with RealtimeClient(config, auth, session) as client:
                backoff = _BACKOFF_MIN_S
                if on_connected is not None:
                    asyncio.create_task(on_connected(client))
                async for event in client.events():
                    if shutdown.is_set():
                        break
                    await on_event(event)
        except asyncio.CancelledError:
            raise
        except (websockets.ConnectionClosed, OSError) as exc:
            log.warning("connection lost, will retry: %s", type(exc).__name__)
        except Exception:
            log.exception("unexpected realtime client error")

        if shutdown.is_set():
            break
        jitter = random.uniform(0, backoff * 0.25)
        delay = min(backoff + jitter, _BACKOFF_MAX_S)
        log.info("reconnecting in %.1fs", delay)
        try:
            await asyncio.wait_for(shutdown.wait(), timeout=delay)
        except asyncio.TimeoutError:
            pass
        backoff = min(backoff * 2, _BACKOFF_MAX_S)
