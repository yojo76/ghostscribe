"""Shared pytest fixtures for the GhostScribe server tests."""

from __future__ import annotations

import io
from pathlib import Path
from typing import Any, Callable

import numpy as np
import pytest
import soundfile as sf
from fastapi.testclient import TestClient

REPO_SERVER_DIR = Path(__file__).resolve().parent.parent
SILENCE_WAV = REPO_SERVER_DIR / "assets" / "silence_1s.wav"

_ENV_VARS = (
    "GHOSTSCRIBE_HOST",
    "GHOSTSCRIBE_PORT",
    "GHOSTSCRIBE_MODEL",
    "GHOSTSCRIBE_DEVICE",
    "GHOSTSCRIBE_COMPUTE_TYPE",
    "GHOSTSCRIBE_LOG_PATH",
    "GHOSTSCRIBE_MAX_UPLOAD_MB",
    "GHOSTSCRIBE_AUTH_TOKEN",
)


@pytest.fixture
def silence_wav_path() -> Path:
    assert SILENCE_WAV.exists(), f"missing fixture asset: {SILENCE_WAV}"
    return SILENCE_WAV


@pytest.fixture
def silence_wav_bytes(silence_wav_path: Path) -> bytes:
    return silence_wav_path.read_bytes()


@pytest.fixture
def tiny_bad_bytes() -> bytes:
    """Not a valid audio container — soundfile must refuse it."""
    return b"not a wav file at all, just garbage"


@pytest.fixture
def oversized_wav_bytes() -> bytes:
    """A syntactically valid WAV just over 1 MB — use with max_bytes=1 MB."""
    samples = np.zeros(16_000 * 60, dtype=np.int16)  # 60 s of silence, ~1.9 MB PCM
    buf = io.BytesIO()
    sf.write(buf, samples, 16_000, subtype="PCM_16", format="WAV")
    return buf.getvalue()


class FakeEngine:
    """Stand-in for ``InferenceEngine`` — no CUDA, no whisper."""

    def __init__(self, settings: Any) -> None:
        self._settings = settings
        self.ready: bool = False
        self.raise_on_transcribe: Exception | None = None
        self.calls: list[dict[str, Any]] = []

    async def warmup(self) -> None:
        self.ready = True

    async def transcribe(
        self,
        audio_bytes: bytes,
        *,
        language: str | None,
        translate: bool,
    ) -> dict[str, Any]:
        self.calls.append(
            {
                "bytes": len(audio_bytes),
                "language": language,
                "translate": translate,
            }
        )
        if self.raise_on_transcribe is not None:
            raise self.raise_on_transcribe
        return {
            "text": "hello world",
            "language": language or "en",
            "language_probability": 0.99,
        }


@pytest.fixture
def make_client(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> Callable[..., TestClient]:
    """Factory that builds a TestClient with ``FakeEngine`` and chosen env.

    Usage:
        client = make_client(env={"GHOSTSCRIBE_AUTH_TOKEN": "x"})
        client.post("/v1/en", files={...})

    The returned TestClient is NOT entered; callers should wrap it in
    ``with make_client() as client`` when they need the lifespan to run
    (e.g., routes that depend on ``app.state.engine``).
    """
    from ghostscribe_server import app as app_module

    def _factory(env: dict[str, str] | None = None) -> TestClient:
        # Scrub any stray env so host config doesn't leak into tests.
        for var in _ENV_VARS:
            monkeypatch.delenv(var, raising=False)
        merged: dict[str, str] = {
            "GHOSTSCRIBE_LOG_PATH": str(tmp_path / "test.log"),
        }
        if env:
            merged.update(env)
        for k, v in merged.items():
            monkeypatch.setenv(k, v)
        monkeypatch.setattr(app_module, "InferenceEngine", FakeEngine)
        return TestClient(app_module.app)

    return _factory
