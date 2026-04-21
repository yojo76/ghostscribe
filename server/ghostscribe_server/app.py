"""FastAPI application for the GhostScribe STT server.

Routes (all under ``/v1``):

* ``GET  /v1/health``  -- liveness + readiness probe.
* ``POST /v1/en``      -- English audio -> English text.
* ``POST /v1/auto``    -- autodetect language, no translation.
* ``GET  /metrics``    -- Prometheus metrics (no auth required).

All transcription endpoints accept a multipart form field named ``audio``
and return ``{text, language, language_probability}``.

Run with::

    uvicorn ghostscribe_server.app:app --host 0.0.0.0 --port 5005 --workers 1

``--workers 1`` is intentional: multiple workers would load the model N
times and blow out VRAM.
"""

from __future__ import annotations

import logging
import time
from contextlib import asynccontextmanager
from typing import Annotated

from fastapi import Depends, FastAPI, File, Header, HTTPException, Request, UploadFile, status
from fastapi.responses import JSONResponse, Response

from . import __version__
from .audio import read_upload
from .config import Settings, load_settings
from .inference import InferenceEngine
from .logging_setup import configure_logging
from .metrics import (
    AUDIO_DURATION,
    INFERENCE_DURATION,
    REQUEST_COUNT,
    REQUEST_DURATION,
    metrics_response,
)


@asynccontextmanager
async def lifespan(app: FastAPI):
    settings = load_settings()
    log = configure_logging(settings.log_path)
    log.info(
        "GhostScribe server v%s starting on %s:%d",
        __version__,
        settings.host,
        settings.port,
    )
    log.info(
        "Config: model=%s device=%s compute_type=%s max_upload_mb=%d auth=%s",
        settings.model_name,
        settings.device,
        settings.compute_type,
        settings.max_upload_mb,
        "on" if settings.auth_required else "off",
    )

    engine = InferenceEngine(settings)
    app.state.settings = settings
    app.state.engine = engine

    await engine.warmup()
    log.info("Server is ready to accept traffic.")

    try:
        yield
    finally:
        log.info("GhostScribe server shutting down.")


app = FastAPI(
    title="GhostScribe STT",
    version=__version__,
    lifespan=lifespan,
    docs_url="/docs",
    redoc_url=None,
)

log = logging.getLogger("ghostscribe.app")


async def auth_dep(
    request: Request,
    x_auth_token: Annotated[str | None, Header(alias="X-Auth-Token")] = None,
) -> None:
    settings: Settings = request.app.state.settings
    if not settings.auth_required:
        return
    if x_auth_token != settings.auth_token:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="invalid or missing X-Auth-Token",
        )


@app.get("/v1/health")
async def health(request: Request) -> JSONResponse:
    settings: Settings = request.app.state.settings
    engine: InferenceEngine = request.app.state.engine
    return JSONResponse(
        {
            "status": "ok",
            "ready": engine.ready,
            "model": settings.model_name,
            "device": settings.device,
            "compute_type": settings.compute_type,
            "version": __version__,
        }
    )


@app.get("/metrics")
async def prometheus_metrics() -> Response:
    body, content_type = metrics_response()
    return Response(content=body, media_type=content_type)


async def _do_transcribe(
    request: Request,
    upload: UploadFile,
    *,
    language: str | None,
    translate: bool,
    label: str,
    endpoint: str,
) -> dict:
    settings: Settings = request.app.state.settings
    engine: InferenceEngine = request.app.state.engine

    if not engine.ready:
        REQUEST_COUNT.labels(endpoint=endpoint, status="503").inc()
        raise HTTPException(
            status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
            detail="server is still warming up; try again shortly",
        )

    t0 = time.perf_counter()
    audio_bytes, info = await read_upload(upload, max_bytes=settings.max_upload_bytes)
    read_ms = (time.perf_counter() - t0) * 1000

    AUDIO_DURATION.observe(info.duration_s)

    t1 = time.perf_counter()
    try:
        result = await engine.transcribe(
            audio_bytes, language=language, translate=translate
        )
    except Exception as exc:
        REQUEST_COUNT.labels(endpoint=endpoint, status="500").inc()
        log.exception("Inference failed for %s", label)
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"inference failed: {exc}",
        ) from exc
    infer_s = time.perf_counter() - t1
    total_s = time.perf_counter() - t0

    INFERENCE_DURATION.observe(infer_s)
    REQUEST_DURATION.labels(endpoint=endpoint).observe(total_s)
    REQUEST_COUNT.labels(endpoint=endpoint, status="200").inc()

    log.info(
        "%s ok: %.0f kB, %.2fs audio, read=%.0fms infer=%.0fms lang=%s(%.2f) text=%r",
        label,
        len(audio_bytes) / 1024,
        info.duration_s,
        read_ms,
        infer_s * 1000,
        result["language"],
        result["language_probability"],
        result["text"][:80],
    )
    return result


@app.post("/v1/en", dependencies=[Depends(auth_dep)])
async def transcribe_en(
    request: Request, audio: Annotated[UploadFile, File(...)]
) -> dict:
    """English audio in, English text out."""
    return await _do_transcribe(
        request, audio, language="en", translate=False, label="EN", endpoint="/v1/en"
    )


@app.post("/v1/auto", dependencies=[Depends(auth_dep)])
async def transcribe_auto(
    request: Request, audio: Annotated[UploadFile, File(...)]
) -> dict:
    """Autodetect language, transcribe (no translation)."""
    return await _do_transcribe(
        request, audio, language=None, translate=False, label="AUTO", endpoint="/v1/auto"
    )
