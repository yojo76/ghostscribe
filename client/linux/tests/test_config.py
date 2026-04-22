"""Unit tests for ghostscribe_client.config."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from ghostscribe_client.config import ClientConfig, DEFAULTS, load_config


@pytest.fixture(autouse=True)
def isolated_env(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Keep load_config from reading the user's real ~/.config or CWD."""
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "xdg"))
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    monkeypatch.chdir(tmp_path)


def test_defaults_when_no_config_file() -> None:
    cfg = load_config(None)
    assert cfg.server_url == DEFAULTS["server_url"]
    assert cfg.endpoint == DEFAULTS["endpoint"]
    assert cfg.trigger == DEFAULTS["trigger"]
    assert cfg.one_key_trigger == ""
    assert cfg.auth_token == ""
    assert cfg.input_device == ""
    assert cfg.audio_format == "flac"
    assert cfg.auto_paste is True
    assert cfg.paste_delay_ms == 50
    assert cfg.source_path is None


def test_load_from_explicit_path(sample_config_path: Path) -> None:
    cfg = load_config(sample_config_path)
    assert cfg.server_url == "http://example.internal:5005"
    assert cfg.endpoint == "/v1/en"
    assert cfg.trigger == "key:ctrl+shift+g"
    assert cfg.one_key_trigger == "key:ctrl"
    assert cfg.auth_token == "s3cret"
    assert cfg.input_device == "USB Audio Device"
    assert cfg.audio_format == "wav"
    assert cfg.auto_paste is False
    assert cfg.paste_delay_ms == 120
    assert cfg.source_path == sample_config_path


def test_url_property_composes_cleanly() -> None:
    cfg = ClientConfig(
        server_url="http://h:5005/",
        endpoint="/v1/en",
        trigger="mouse:x2",
    )
    assert cfg.url == "http://h:5005/v1/en"

    cfg2 = ClientConfig(
        server_url="http://h:5005",
        endpoint="v1/en",
        trigger="mouse:x2",
    )
    assert cfg2.url == "http://h:5005/v1/en"


def test_has_auth_toggle() -> None:
    assert (
        ClientConfig(server_url="x", endpoint="y", trigger="z", auth_token="").has_auth
        is False
    )
    assert (
        ClientConfig(server_url="x", endpoint="y", trigger="z", auth_token="t").has_auth
        is True
    )


def test_audio_format_lowercased(tmp_path: Path) -> None:
    p = tmp_path / "config.toml"
    p.write_text('server_url="x"\nendpoint="/y"\ntrigger="mouse:x2"\naudio_format="FLAC"\n')
    cfg = load_config(p)
    assert cfg.audio_format == "flac"


@pytest.mark.parametrize("raw,expected", [
    ("true", True), ("True", True), ("on", True), ("YES", True), ("1", True),
    ("false", False), ("FALSE", False), ("off", False), ("no", False), ("0", False),
])
def test_auto_paste_string_coercion(
    tmp_path: Path, raw: str, expected: bool
) -> None:
    p = tmp_path / "config.toml"
    p.write_text(
        f'server_url="x"\nendpoint="/y"\ntrigger="mouse:x2"\nauto_paste="{raw}"\n'
    )
    cfg = load_config(p)
    assert cfg.auto_paste is expected


def test_auto_paste_bogus_value_raises(tmp_path: Path) -> None:
    p = tmp_path / "config.toml"
    p.write_text(
        'server_url="x"\nendpoint="/y"\ntrigger="mouse:x2"\nauto_paste="maybe"\n'
    )
    with pytest.raises(ValueError, match="auto_paste"):
        load_config(p)


def test_xdg_path_found(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    xdg_dir = tmp_path / "xdg" / "ghostscribe"
    xdg_dir.mkdir(parents=True)
    cfg_path = xdg_dir / "config.toml"
    cfg_path.write_text('server_url="http://xdg"\nendpoint="/v1/auto"\ntrigger="mouse:x2"\n')
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "xdg"))
    cfg = load_config(None)
    assert cfg.server_url == "http://xdg"
    assert cfg.source_path == cfg_path


def test_cwd_config_as_last_resort(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Ensure neither XDG nor HOME config exists.
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "no-xdg"))
    monkeypatch.setenv("HOME", str(tmp_path / "no-home"))
    monkeypatch.chdir(tmp_path)
    cwd_cfg = tmp_path / "config.toml"
    cwd_cfg.write_text('server_url="http://cwd"\nendpoint="/v1/auto"\ntrigger="mouse:x2"\n')
    cfg = load_config(None)
    assert cfg.server_url == "http://cwd"
    assert cfg.source_path == cwd_cfg
