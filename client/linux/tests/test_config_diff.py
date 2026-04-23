"""Tests for ``ghostscribe_client.config.diff`` and friends."""

from __future__ import annotations

from pathlib import Path

import pytest

from ghostscribe_client.config import (
    ClientConfig,
    COLD_KEYS,
    ConfigDiff,
    DEFAULT_CONFIG_TOML,
    DEFAULTS,
    HOT_KEYS,
    diff,
    load_from,
)


def _base() -> ClientConfig:
    return ClientConfig(
        server_url=str(DEFAULTS["server_url"]),
        endpoint=str(DEFAULTS["endpoint"]),
        trigger=str(DEFAULTS["trigger"]),
        one_key_trigger="",
        auth_token="",
        input_device="",
        audio_format="flac",
        auto_paste=True,
        paste_delay_ms=50,
    )


def test_keys_partition_is_disjoint_and_complete() -> None:
    """HOT_KEYS and COLD_KEYS must be disjoint and together cover every
    user-visible config field. ``source_path`` is internal and excluded."""
    hot = set(HOT_KEYS)
    cold = set(COLD_KEYS)
    assert hot.isdisjoint(cold)
    every = hot | cold
    field_names = {
        f for f in ClientConfig.__dataclass_fields__ if f != "source_path"
    }
    assert every == field_names, (
        f"missing from HOT/COLD: {field_names - every}; "
        f"extra: {every - field_names}"
    )


def test_diff_identical_returns_empty() -> None:
    cfg = _base()
    d = diff(cfg, cfg)
    assert d == ConfigDiff()
    assert d.is_empty()
    assert not d.requires_restart()


def test_diff_ignores_source_path() -> None:
    a = _base()
    b = ClientConfig(**{**a.__dict__, "source_path": Path("/tmp/x.toml")})
    assert diff(a, b).is_empty()


def test_diff_classifies_hot_change_only() -> None:
    a = _base()
    b = ClientConfig(**{**a.__dict__, "auth_token": "new"})
    d = diff(a, b)
    assert d.hot_changed == ("auth_token",)
    assert d.cold_changed == ()
    assert not d.requires_restart()


def test_diff_classifies_cold_change_only() -> None:
    a = _base()
    b = ClientConfig(**{**a.__dict__, "trigger": "key:ctrl+m"})
    d = diff(a, b)
    assert d.hot_changed == ()
    assert d.cold_changed == ("trigger",)
    assert d.requires_restart()


def test_diff_reports_mixed_changes_in_canonical_order() -> None:
    a = _base()
    b = ClientConfig(
        **{
            **a.__dict__,
            "server_url": "http://other:5005",
            "auto_paste": False,
            "audio_format": "wav",
            "input_device": "USB Mic",
        }
    )
    d = diff(a, b)
    # Order matches HOT_KEYS / COLD_KEYS declaration so UI strings stay stable.
    assert d.hot_changed == ("server_url", "auto_paste")
    assert d.cold_changed == ("input_device", "audio_format")
    assert d.requires_restart()


def test_default_config_toml_loads_via_load_from(tmp_path: Path) -> None:
    """The seed template must be parseable by load_from() so the user
    isn't immediately greeted with a parse error after first-run seeding."""
    target = tmp_path / "config.toml"
    target.write_text(DEFAULT_CONFIG_TOML)
    cfg = load_from(target)
    # Everything is commented in the template, so we should land on
    # built-in defaults.
    assert cfg.server_url == DEFAULTS["server_url"]
    assert cfg.endpoint == DEFAULTS["endpoint"]
    assert cfg.trigger == DEFAULTS["trigger"]
    assert cfg.source_path == target


def test_load_from_propagates_parse_error(tmp_path: Path) -> None:
    p = tmp_path / "broken.toml"
    p.write_text('not = a = valid = toml')
    with pytest.raises(Exception):  # noqa: PT011 - tomllib version-dependent
        load_from(p)
