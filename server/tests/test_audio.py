"""Unit tests for ghostscribe_server.audio."""

from __future__ import annotations

import io
from typing import Any

import pytest
from fastapi import HTTPException, UploadFile

from ghostscribe_server.audio import AudioInfo, read_upload


def _upload_from(data: bytes, filename: str = "x.wav") -> UploadFile:
    return UploadFile(filename=filename, file=io.BytesIO(data))


async def test_valid_wav_parses(silence_wav_bytes: bytes) -> None:
    payload, info = await read_upload(
        _upload_from(silence_wav_bytes), max_bytes=10 * 1024 * 1024
    )
    assert payload == silence_wav_bytes
    assert isinstance(info, AudioInfo)
    assert info.samplerate == 16_000
    assert info.channels == 1
    # silence_1s.wav is ~1 s of audio; allow a tight tolerance
    assert 0.9 <= info.duration_s <= 1.1


async def test_empty_payload_rejected() -> None:
    with pytest.raises(HTTPException) as exc:
        await read_upload(_upload_from(b""), max_bytes=1024)
    assert exc.value.status_code == 400
    assert "empty" in exc.value.detail.lower()


async def test_oversized_payload_rejected(oversized_wav_bytes: bytes) -> None:
    with pytest.raises(HTTPException) as exc:
        await read_upload(_upload_from(oversized_wav_bytes), max_bytes=1024)
    assert exc.value.status_code == 413
    assert "exceeds" in exc.value.detail.lower()


async def test_garbage_bytes_rejected(tiny_bad_bytes: bytes) -> None:
    with pytest.raises(HTTPException) as exc:
        await read_upload(_upload_from(tiny_bad_bytes), max_bytes=1024)
    assert exc.value.status_code == 400
    assert "could not parse" in exc.value.detail.lower()


def test_audio_info_duration_math() -> None:
    info = AudioInfo(
        samplerate=16_000, channels=1, frames=24_000, format="WAV", subtype="PCM_16"
    )
    assert info.duration_s == pytest.approx(1.5)


def test_audio_info_duration_zero_samplerate() -> None:
    info = AudioInfo(
        samplerate=0, channels=1, frames=100, format="WAV", subtype="PCM_16"
    )
    assert info.duration_s == 0.0
