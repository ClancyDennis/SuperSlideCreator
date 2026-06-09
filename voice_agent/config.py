"""Environment-driven config. Never log raw values from here."""
from __future__ import annotations

import os
from dataclasses import dataclass
from urllib.parse import urlencode, urlparse


_VALID_REASONING_EFFORTS = frozenset({"minimal", "low", "medium", "high"})


@dataclass(frozen=True)
class RealtimeConfig:
    endpoint: str
    deployment: str
    api_version: str
    api_key: str | None
    voice: str
    instructions: str
    log_level: str
    # Slide-building worker (chat-completions, e.g. gpt-5.4) — a separate
    # deployment from the realtime voice model.
    slide_deployment: str
    slide_api_version: str
    # Image generation (e.g. gpt-image-2) — same resource, images/generations API.
    image_deployment: str
    image_api_version: str
    # Empty string disables the field entirely (so v1 deployments don't
    # reject the unknown key). For realtime-2, set REALTIME_REASONING_EFFORT
    # to one of: minimal, low, medium, high.
    reasoning_effort: str

    @property
    def ws_url(self) -> str:
        host = urlparse(self.endpoint).netloc
        if not host:
            raise ValueError("AZURE_OPENAI_ENDPOINT must be a full https URL")
        query = urlencode({
            "api-version": self.api_version,
            "deployment": self.deployment,
        })
        return f"wss://{host}/openai/realtime?{query}"

    @property
    def dashboard_chat_url(self) -> str:
        """Chat-completions URL for the slide-building worker. (Name kept for
        compatibility with the agent code that consumes it.)"""
        host = urlparse(self.endpoint).netloc
        if not host:
            raise ValueError("AZURE_OPENAI_ENDPOINT must be a full https URL")
        return (
            f"https://{host}/openai/deployments/{self.slide_deployment}"
            f"/chat/completions?api-version={self.slide_api_version}"
        )

    @property
    def image_gen_url(self) -> str:
        """images/generations URL for the image model (e.g. gpt-image-2)."""
        host = urlparse(self.endpoint).netloc
        if not host:
            raise ValueError("AZURE_OPENAI_ENDPOINT must be a full https URL")
        return (
            f"https://{host}/openai/deployments/{self.image_deployment}"
            f"/images/generations?api-version={self.image_api_version}"
        )


def load_config() -> RealtimeConfig:
    try:
        from dotenv import load_dotenv
        load_dotenv()
    except ImportError:
        pass

    endpoint = os.environ["AZURE_OPENAI_ENDPOINT"].rstrip("/")
    deployment = os.environ["AZURE_OPENAI_DEPLOYMENT"]
    api_version = os.environ.get("AZURE_OPENAI_API_VERSION", "2024-10-01-preview")
    api_key = os.environ.get("AZURE_OPENAI_API_KEY") or None
    voice = os.environ.get("REALTIME_VOICE", "alloy")
    instructions = os.environ.get("REALTIME_INSTRUCTIONS", "You are a helpful assistant.")
    log_level = os.environ.get("LOG_LEVEL", "INFO")

    if not api_key:
        raise RuntimeError(
            "AZURE_OPENAI_API_KEY not set. For production, prefer AAD via azure-identity."
        )

    # Accept the old DASHBOARD_* names as fallbacks so existing .env files keep working.
    slide_deployment = (
        os.environ.get("SLIDE_AZURE_OPENAI_DEPLOYMENT")
        or os.environ.get("DASHBOARD_AZURE_OPENAI_DEPLOYMENT")
        or "gpt-5.4"
    )
    slide_api_version = (
        os.environ.get("SLIDE_AZURE_OPENAI_API_VERSION")
        or os.environ.get("DASHBOARD_AZURE_OPENAI_API_VERSION")
        or "2024-10-21"
    )

    image_deployment = os.environ.get("IMAGE_AZURE_OPENAI_DEPLOYMENT", "gpt-image-2")
    image_api_version = os.environ.get("IMAGE_AZURE_OPENAI_API_VERSION", "2025-04-01-preview")

    reasoning_effort = os.environ.get("REALTIME_REASONING_EFFORT", "").strip().lower()
    if reasoning_effort and reasoning_effort not in _VALID_REASONING_EFFORTS:
        raise RuntimeError(
            f"REALTIME_REASONING_EFFORT must be one of {sorted(_VALID_REASONING_EFFORTS)} "
            f"or empty; got {reasoning_effort!r}"
        )

    return RealtimeConfig(
        endpoint=endpoint,
        deployment=deployment,
        api_version=api_version,
        api_key=api_key,
        voice=voice,
        instructions=instructions,
        log_level=log_level,
        slide_deployment=slide_deployment,
        slide_api_version=slide_api_version,
        image_deployment=image_deployment,
        image_api_version=image_api_version,
        reasoning_effort=reasoning_effort,
    )
