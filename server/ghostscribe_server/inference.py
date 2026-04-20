"""Wrapper around ``faster_whisper.WhisperModel``.

Responsibilities:

* Load the model once at startup (stays resident in VRAM).
* Serialise GPU access with a single ``asyncio.Semaphore`` because
  ``faster-whisper`` is not safe to call re-entrantly on the same model.
* Run the blocking ``transcribe`` call via ``asyncio.to_thread`` so the
  FastAPI event loop stays responsive.
* Expose a ``warmup()`` coroutine that runs a synthetic silence
  transcription to pay the CUDA/cuDNN autotune cost before the first real
  request. ``ready`` flips true only after warm-up completes.
"""

from __future__ import annotations

import asyncio
import io
import logging
import time
from pathlib import Path
from typing import Any

from faster_whisper import WhisperModel

from .config import Settings

log = logging.getLogger("ghostscribe.inference")

_ASSETS_DIR = Path(__file__).resolve().parent.parent / "assets"
_SILENCE_WAV_PATH = _ASSETS_DIR / "silence_1s.wav"


class InferenceEngine:
    """GPU-resident Whisper inference engine."""

    def __init__(self, settings: Settings) -> None:
        self._settings = settings
        self._sem = asyncio.Semaphore(1)
        self.ready: bool = False

        log.info(
            "Loading %s on %s (compute_type=%s)...",
            settings.model_name,
            settings.device,
            settings.compute_type,
        )
        t0 = time.perf_counter()
        self.model = WhisperModel(
            settings.model_name,
            device=settings.device,
            compute_type=settings.compute_type,
        )
        log.info("Model loaded in %.2fs", time.perf_counter() - t0)

    async def warmup(self) -> None:
        """Run one synthetic inference so the first real request is fast."""
        try:
            audio_bytes = _SILENCE_WAV_PATH.read_bytes()
        except OSError as exc:
            log.warning(
                "Warm-up skipped: could not read %s (%s). "
                "The first real request will pay the CUDA/cuDNN autotune cost.",
                _SILENCE_WAV_PATH,
                exc,
            )
            self.ready = True
            return

        log.info("Running warm-up transcription...")
        t0 = time.perf_counter()
        try:
            await self.transcribe(audio_bytes, language="en", translate=False)
        except Exception:
            log.exception("Warm-up transcription failed")
            raise
        log.info("Warm-up complete in %.2fs", time.perf_counter() - t0)
        self.ready = True

    async def transcribe(
        self,
        audio_bytes: bytes,
        *,
        language: str | None,
        translate: bool,
    ) -> dict[str, Any]:
        """Transcribe ``audio_bytes``. ``language=None`` enables autodetection."""
        async with self._sem:
            return await asyncio.to_thread(
                self._run_blocking, audio_bytes, language, translate
            )

    def _run_blocking(
        self,
        audio_bytes: bytes,
        language: str | None,
        translate: bool,
    ) -> dict[str, Any]:
        task = "translate" if translate else "transcribe"
        segments, info = self.model.transcribe(
            io.BytesIO(audio_bytes),
            language=language,
            task=task,
            beam_size=5,
            vad_filter=True,
            vad_parameters={"min_silence_duration_ms": 300},
        )
        text = " ".join(s.text.strip() for s in segments).strip()
        return {
            "text": text,
            "language": info.language,
            "language_probability": round(float(info.language_probability), 3),
        }
