"""Unit tests for audio encoding in ghostscribe_client.__main__."""

from __future__ import annotations

import io

import numpy as np
import pytest
import soundfile as sf

from ghostscribe_client.__main__ import SAMPLE_RATE, encode_audio


def _tone(seconds: float = 0.25, freq: int = 440) -> np.ndarray:
    t = np.arange(int(SAMPLE_RATE * seconds)) / SAMPLE_RATE
    samples = (np.sin(2 * np.pi * freq * t) * 16_000).astype(np.int16)
    return samples


def test_flac_roundtrip_decodes_to_input() -> None:
    tone = _tone()
    payload, filename, mime = encode_audio(tone, "flac")
    assert filename == "recording.flac"
    assert mime == "audio/flac"
    assert payload[:4] == b"fLaC", "FLAC stream must start with fLaC magic"

    decoded, sr = sf.read(io.BytesIO(payload), dtype="int16")
    assert sr == SAMPLE_RATE
    np.testing.assert_array_equal(decoded, tone)


def test_wav_roundtrip_decodes_to_input() -> None:
    tone = _tone()
    payload, filename, mime = encode_audio(tone, "wav")
    assert filename == "recording.wav"
    assert mime == "audio/wav"
    assert payload[:4] == b"RIFF"

    decoded, sr = sf.read(io.BytesIO(payload), dtype="int16")
    assert sr == SAMPLE_RATE
    np.testing.assert_array_equal(decoded, tone)


def test_unknown_format_raises() -> None:
    with pytest.raises(ValueError, match="unknown audio_format"):
        encode_audio(_tone(), "mp3")
