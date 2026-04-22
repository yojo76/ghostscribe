"""Integration tests for the ``auth_dep`` FastAPI dependency."""

from __future__ import annotations


def test_auth_off_allows_request(make_client, silence_wav_bytes) -> None:
    with make_client() as client:
        r = client.post(
            "/v1/en",
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
    assert r.status_code == 200


def test_auth_on_rejects_missing_token(make_client, silence_wav_bytes) -> None:
    with make_client({"GHOSTSCRIBE_AUTH_TOKEN": "secret"}) as client:
        r = client.post(
            "/v1/en",
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
    assert r.status_code == 401
    assert "X-Auth-Token" in r.json()["detail"]


def test_auth_on_rejects_wrong_token(make_client, silence_wav_bytes) -> None:
    with make_client({"GHOSTSCRIBE_AUTH_TOKEN": "secret"}) as client:
        r = client.post(
            "/v1/en",
            headers={"X-Auth-Token": "wrong"},
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
    assert r.status_code == 401


def test_auth_on_accepts_correct_token(make_client, silence_wav_bytes) -> None:
    with make_client({"GHOSTSCRIBE_AUTH_TOKEN": "secret"}) as client:
        r = client.post(
            "/v1/en",
            headers={"X-Auth-Token": "secret"},
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
    assert r.status_code == 200


def test_health_does_not_require_auth(make_client) -> None:
    with make_client({"GHOSTSCRIBE_AUTH_TOKEN": "secret"}) as client:
        r = client.get("/v1/health")
    assert r.status_code == 200


def test_metrics_does_not_require_auth(make_client) -> None:
    with make_client({"GHOSTSCRIBE_AUTH_TOKEN": "secret"}) as client:
        r = client.get("/metrics")
    assert r.status_code == 200
