"""Audio payload validation.

The endpoints accept any container format ``libsndfile`` can read (WAV, FLAC,
OGG, ...). We do a cheap ``soundfile.info`` probe up front so a malformed
upload returns a clean HTTP 400 instead of blowing up deep inside ffmpeg
during the ``faster-whisper`` call.
"""

from __future__ import annotations

import io
import logging
from dataclasses import dataclass

import soundfile as sf
from fastapi import HTTPException, UploadFile, status

log = logging.getLogger("ghostscribe.audio")


@dataclass(frozen=True)
class AudioInfo:
    samplerate: int
    channels: int
    frames: int
    format: str
    subtype: str

    @property
    def duration_s(self) -> float:
        return self.frames / float(self.samplerate) if self.samplerate else 0.0


async def read_upload(
    upload: UploadFile,
    *,
    max_bytes: int,
) -> tuple[bytes, AudioInfo]:
    """Read an ``UploadFile`` into memory and validate it as audio.

    Raises ``HTTPException(400)`` on invalid/corrupt audio and
    ``HTTPException(413)`` when the payload exceeds ``max_bytes``.
    """
    audio_bytes = await upload.read()
    size = len(audio_bytes)
    if size == 0:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="empty audio payload",
        )
    if size > max_bytes:
        raise HTTPException(
            status_code=status.HTTP_413_REQUEST_ENTITY_TOO_LARGE,
            detail=f"audio payload {size} bytes exceeds limit of {max_bytes} bytes",
        )

    try:
        info = sf.info(io.BytesIO(audio_bytes))
    except Exception as exc:  # soundfile raises a bare RuntimeError
        log.warning("Rejecting upload: soundfile could not parse it (%s)", exc)
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"could not parse audio: {exc}",
        ) from exc

    return audio_bytes, AudioInfo(
        samplerate=int(info.samplerate),
        channels=int(info.channels),
        frames=int(info.frames),
        format=str(info.format),
        subtype=str(info.subtype),
    )
