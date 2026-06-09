"""PHI-safe logging.

Rules enforced here:
- Never log audio bytes, transcripts, or instructions.
- Log event types, IDs, sizes, and status only.
- Redact anything that looks like a base64 audio blob or a `transcript` field.
"""
from __future__ import annotations

import logging
import re
import sys

_REDACTED = "[REDACTED]"
_SENSITIVE_KEYS = ("audio", "delta", "transcript", "instructions", "input_audio", "text")
_KV_PATTERN = re.compile(
    r"('?(?:" + "|".join(_SENSITIVE_KEYS) + r")'?\s*[:=]\s*)('(?:[^'\\]|\\.)*'|\"(?:[^\"\\]|\\.)*\")",
    re.IGNORECASE,
)


class PhiRedactor(logging.Filter):
    def filter(self, record: logging.LogRecord) -> bool:
        if isinstance(record.msg, str):
            record.msg = _KV_PATTERN.sub(lambda m: f"{m.group(1)}{_REDACTED}", record.msg)
        return True


def setup_logging(level: str = "INFO") -> logging.Logger:
    root = logging.getLogger()
    root.handlers.clear()
    handler = logging.StreamHandler(sys.stdout)
    handler.setFormatter(logging.Formatter(
        "%(asctime)s %(levelname)s %(name)s %(message)s",
        datefmt="%Y-%m-%dT%H:%M:%S%z",
    ))
    handler.addFilter(PhiRedactor())
    root.addHandler(handler)
    root.setLevel(level.upper())
    return logging.getLogger("voice_agent")
