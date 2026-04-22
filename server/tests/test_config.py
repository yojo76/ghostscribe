"""Unit tests for ghostscribe_server.config."""

from __future__ import annotations

from pathlib import Path

import pytest

from ghostscribe_server.config import Settings, load_settings

ENV_VARS = [
    "GHOSTSCRIBE_HOST",
    "GHOSTSCRIBE_PORT",
    "GHOSTSCRIBE_MODEL",
    "GHOSTSCRIBE_DEVICE",
    "GHOSTSCRIBE_COMPUTE_TYPE",
    "GHOSTSCRIBE_LOG_PATH",
    "GHOSTSCRIBE_MAX_UPLOAD_MB",
    "GHOSTSCRIBE_AUTH_TOKEN",
]


@pytest.fixture(autouse=True)
def clean_env(monkeypatch: pytest.MonkeyPatch) -> None:
    for var in ENV_VARS:
        monkeypatch.delenv(var, raising=False)


def test_defaults_when_no_env() -> None:
    s = load_settings()
    assert s.host == "0.0.0.0"
    assert s.port == 5005
    assert s.model_name == "large-v3-turbo"
    assert s.device == "cuda"
    assert s.compute_type == "int8_float16"
    assert s.log_path == Path("logs") / "ghostscribe_server.log"
    assert s.max_upload_mb == 25
    assert s.auth_token is None


def test_env_overrides(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GHOSTSCRIBE_HOST", "127.0.0.1")
    monkeypatch.setenv("GHOSTSCRIBE_PORT", "9000")
    monkeypatch.setenv("GHOSTSCRIBE_MODEL", "small")
    monkeypatch.setenv("GHOSTSCRIBE_DEVICE", "cpu")
    monkeypatch.setenv("GHOSTSCRIBE_COMPUTE_TYPE", "int8")
    monkeypatch.setenv("GHOSTSCRIBE_LOG_PATH", "/tmp/gs.log")
    monkeypatch.setenv("GHOSTSCRIBE_MAX_UPLOAD_MB", "10")
    monkeypatch.setenv("GHOSTSCRIBE_AUTH_TOKEN", "s3cret")

    s = load_settings()
    assert s.host == "127.0.0.1"
    assert s.port == 9000
    assert s.model_name == "small"
    assert s.device == "cpu"
    assert s.compute_type == "int8"
    assert s.log_path == Path("/tmp/gs.log")
    assert s.max_upload_mb == 10
    assert s.auth_token == "s3cret"


def test_empty_string_env_falls_back_to_default(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GHOSTSCRIBE_HOST", "")
    monkeypatch.setenv("GHOSTSCRIBE_AUTH_TOKEN", "")
    s = load_settings()
    assert s.host == "0.0.0.0"
    assert s.auth_token is None


def test_non_integer_port_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GHOSTSCRIBE_PORT", "not-a-number")
    with pytest.raises(ValueError, match="GHOSTSCRIBE_PORT"):
        load_settings()


def test_max_upload_bytes_derived() -> None:
    s = Settings(
        host="0.0.0.0",
        port=5005,
        model_name="m",
        device="cpu",
        compute_type="int8",
        log_path=Path("x.log"),
        max_upload_mb=7,
        auth_token=None,
    )
    assert s.max_upload_bytes == 7 * 1024 * 1024


def test_auth_required_toggle() -> None:
    base = dict(
        host="0.0.0.0",
        port=5005,
        model_name="m",
        device="cpu",
        compute_type="int8",
        log_path=Path("x.log"),
        max_upload_mb=1,
    )
    assert Settings(**base, auth_token=None).auth_required is False
    assert Settings(**base, auth_token="x").auth_required is True
