# Multi-stage build: a build layer with compilers, a slim runtime layer.
# Pinned to 3.13 because 3.14's ensurepip is still flaky on some wheels.

# -------- build --------
FROM python:3.13-slim-bookworm AS build

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1

WORKDIR /build

# Only what we need to build wheels; audio/SSL libs are already in the slim image.
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential \
 && rm -rf /var/lib/apt/lists/*

COPY requirements.txt .
RUN python -m venv /opt/venv \
 && /opt/venv/bin/pip install --upgrade pip \
 && /opt/venv/bin/pip install -r requirements.txt

# -------- runtime --------
FROM python:3.13-slim-bookworm AS runtime

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PATH="/opt/venv/bin:$PATH" \
    PORT=8765 \
    LOG_LEVEL=INFO

# ca-certificates so the container trusts public roots. Corporate CAs for a
# TLS-inspecting proxy must be mounted at /usr/local/share/ca-certificates/
# at deploy time and the image re-run with `update-ca-certificates`.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 app \
 && useradd  --system --uid 10001 --gid app --home /app --shell /usr/sbin/nologin app

WORKDIR /app

COPY --from=build /opt/venv /opt/venv
COPY --chown=app:app voice_agent/ ./voice_agent/
COPY --chown=app:app web/ ./web/

USER app

EXPOSE 8765

# Healthcheck hits the relay's own /healthz endpoint. Fails fast: 5s timeout,
# 3 retries, gives the app 20s to boot before probing.
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT}/healthz" || exit 1

# tini as PID 1 so SIGTERM propagates cleanly — important for graceful WS
# drain when the orchestrator rolls the pod.
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["sh", "-c", "exec uvicorn voice_agent.server:app --host 0.0.0.0 --port \"${PORT}\" --log-level \"$(echo \"${LOG_LEVEL}\" | tr A-Z a-z)\" --proxy-headers --forwarded-allow-ips=*"]
