"""Smoke-test entrypoint.

Reads a 24kHz mono PCM16 file from VOICE_INPUT_PCM, streams it to the model,
asks for a response, and writes returned audio to VOICE_OUTPUT_PCM.

No PHI is logged. Use a synthetic sample for CI.
"""
from __future__ import annotations

import argparse
import asyncio
import base64
import logging
import os
import signal
from pathlib import Path

from .auth import ApiKeyAuth
from .client import RealtimeClient, SessionOptions, run_with_reconnect
from .config import load_config
from .logging_setup import setup_logging

log = logging.getLogger("voice_agent.main")

_CHUNK_MS = 40  # ~40ms chunks @24kHz pcm16 = 1920 bytes
_BYTES_PER_SAMPLE = 2
_SAMPLE_RATE = 24000
_CHUNK_BYTES = _SAMPLE_RATE * _BYTES_PER_SAMPLE * _CHUNK_MS // 1000


async def _stream_file(client: RealtimeClient, pcm_path: Path) -> None:
    log.info("streaming audio file bytes=%d", pcm_path.stat().st_size)
    with pcm_path.open("rb") as f:
        while chunk := f.read(_CHUNK_BYTES):
            await client.send_audio_chunk(chunk)
            await asyncio.sleep(_CHUNK_MS / 1000)
    await client.commit_audio()
    await client.request_response()
    log.info("audio committed; awaiting response")


async def _handle_event(event: dict, out_path: Path, done: asyncio.Event) -> None:
    etype = event.get("type", "")
    event_id = event.get("event_id")
    log.info("event type=%s id=%s", etype, event_id)

    if etype == "response.audio.delta":
        audio_b64 = event.get("delta", "")
        if audio_b64:
            with out_path.open("ab") as f:
                f.write(base64.b64decode(audio_b64))
    elif etype == "response.done":
        done.set()
    elif etype == "error":
        # Log the error code/type only, never free-form messages that could carry PHI.
        err = event.get("error", {})
        log.error("realtime error code=%s type=%s", err.get("code"), err.get("type"))
        done.set()


async def _run(args: argparse.Namespace) -> int:
    config = load_config()
    setup_logging(config.log_level)

    pcm_in = Path(args.input)
    pcm_out = Path(args.output)
    if pcm_out.exists():
        pcm_out.unlink()

    session = SessionOptions(voice=config.voice, instructions=config.instructions)
    auth = ApiKeyAuth(config.api_key or "")
    done = asyncio.Event()
    shutdown = asyncio.Event()

    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, shutdown.set)

    async def on_event(event: dict) -> None:
        await _handle_event(event, pcm_out, done)
        if done.is_set():
            shutdown.set()

    async def on_connected(client: RealtimeClient) -> None:
        try:
            await _stream_file(client, pcm_in)
        except Exception:
            log.exception("failed while streaming input file")
            shutdown.set()

    await run_with_reconnect(
        config=config,
        auth=auth,
        session=session,
        on_event=on_event,
        on_connected=on_connected,
        shutdown=shutdown,
    )
    log.info("exited cleanly output=%s size=%d",
             pcm_out, pcm_out.stat().st_size if pcm_out.exists() else 0)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Azure Realtime voice agent smoke test")
    parser.add_argument("--input", default=os.environ.get("VOICE_INPUT_PCM", "input.pcm"),
                        help="Path to 24kHz mono PCM16 file to send")
    parser.add_argument("--output", default=os.environ.get("VOICE_OUTPUT_PCM", "output.pcm"),
                        help="Path to write model audio (24kHz mono PCM16)")
    args = parser.parse_args()
    return asyncio.run(_run(args))


if __name__ == "__main__":
    raise SystemExit(main())
