"""Integration tests for the FastAPI route surface."""

from __future__ import annotations


def test_health_shape(make_client) -> None:
    with make_client() as client:
        r = client.get("/v1/health")
    assert r.status_code == 200
    body = r.json()
    for key in ("status", "ready", "model", "device", "compute_type", "version"):
        assert key in body
    assert body["status"] == "ok"
    assert body["ready"] is True


def test_en_returns_expected_shape(make_client, silence_wav_bytes) -> None:
    with make_client() as client:
        r = client.post(
            "/v1/en",
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
    assert r.status_code == 200
    body = r.json()
    assert body == {
        "text": "hello world",
        "language": "en",
        "language_probability": 0.99,
    }


def test_auto_passes_none_language(make_client, silence_wav_bytes) -> None:
    with make_client() as client:
        r = client.post(
            "/v1/auto",
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
        engine = client.app.state.engine
        assert engine.calls[-1]["language"] is None
        assert engine.calls[-1]["translate"] is False
    assert r.status_code == 200


def test_en_passes_en_language(make_client, silence_wav_bytes) -> None:
    with make_client() as client:
        r = client.post(
            "/v1/en",
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
        engine = client.app.state.engine
        assert engine.calls[-1]["language"] == "en"
    assert r.status_code == 200


def test_not_ready_returns_503(make_client, silence_wav_bytes) -> None:
    with make_client() as client:
        client.app.state.engine.ready = False
        r = client.post(
            "/v1/en",
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
    assert r.status_code == 503
    assert "warming up" in r.json()["detail"].lower()


def test_engine_exception_returns_500(make_client, silence_wav_bytes) -> None:
    with make_client() as client:
        client.app.state.engine.raise_on_transcribe = RuntimeError("boom")
        r = client.post(
            "/v1/en",
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
    assert r.status_code == 500
    assert "boom" in r.json()["detail"]


def test_malformed_audio_returns_400(make_client, tiny_bad_bytes) -> None:
    with make_client() as client:
        r = client.post(
            "/v1/en",
            files={"audio": ("x.wav", tiny_bad_bytes, "audio/wav")},
        )
    assert r.status_code == 400


def test_oversized_audio_returns_413(
    make_client, oversized_wav_bytes
) -> None:
    with make_client({"GHOSTSCRIBE_MAX_UPLOAD_MB": "1"}) as client:
        # oversized_wav is ~1.9 MB; cap of 1 MB must reject.
        r = client.post(
            "/v1/en",
            files={"audio": ("x.wav", oversized_wav_bytes, "audio/wav")},
        )
    assert r.status_code == 413


def test_missing_audio_field_returns_422(make_client) -> None:
    with make_client() as client:
        r = client.post("/v1/en")
    assert r.status_code == 422
