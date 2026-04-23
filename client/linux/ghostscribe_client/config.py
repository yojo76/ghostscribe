"""TOML-driven configuration for the GhostScribe client.

The client looks for a config file in this order:

1. ``--config PATH`` CLI argument.
2. ``$XDG_CONFIG_HOME/ghostscribe/config.toml`` or ``~/.config/ghostscribe/config.toml``.
3. ``./config.toml`` (next to the current working directory).
4. Hard-coded defaults (useful for ``--help`` and smoke tests).
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover - Python 3.10 fallback
    import tomli as tomllib  # type: ignore[no-redef]


# Text seeded into a freshly-created `config.toml` when the user clicks
# "Edit config…" from the tray and no file exists yet. Kept in lock-step
# with DEFAULTS below; the commented defaults double as inline docs.
DEFAULT_CONFIG_TOML: str = """\
# GhostScribe Linux client config
# All keys are optional; commented lines show the built-in defaults.

# server_url      = "http://localhost:5005"
# endpoint        = "/v1/auto"
# auth_token      = ""
# input_device    = ""               # substring match; empty = system default
# trigger         = "mouse:x2"       # push-to-talk chord or mouse button
# one_key_trigger = ""               # optional single-key PTT, e.g. key:ctrl
# audio_format    = "flac"           # "flac" or "wav"
# auto_paste      = true
# paste_delay_ms  = 50
"""


DEFAULTS: dict[str, object] = {
    "server_url": "http://localhost:5005",
    "endpoint": "/v1/auto",
    "trigger": "mouse:x2",
    "one_key_trigger": "",
    "auth_token": "",
    "input_device": "",
    "audio_format": "flac",
    "auto_paste": True,
    "paste_delay_ms": 50,
}


@dataclass(frozen=True)
class ClientConfig:
    server_url: str
    endpoint: str
    trigger: str
    one_key_trigger: str = ""
    auth_token: str = ""
    input_device: str = ""
    audio_format: str = "flac"
    auto_paste: bool = True
    paste_delay_ms: int = 50
    source_path: Path | None = field(default=None, compare=False)

    @property
    def url(self) -> str:
        return self.server_url.rstrip("/") + "/" + self.endpoint.lstrip("/")

    @property
    def has_auth(self) -> bool:
        return bool(self.auth_token)


def _candidate_paths(explicit: Path | None) -> list[Path]:
    if explicit is not None:
        return [explicit]
    paths: list[Path] = []
    xdg = os.environ.get("XDG_CONFIG_HOME")
    if xdg:
        paths.append(Path(xdg) / "ghostscribe" / "config.toml")
    paths.append(Path.home() / ".config" / "ghostscribe" / "config.toml")
    paths.append(Path.cwd() / "config.toml")
    return paths


def _coerce_bool(value: object, *, field_name: str) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        v = value.strip().lower()
        if v in {"true", "yes", "on", "1"}:
            return True
        if v in {"false", "no", "off", "0"}:
            return False
    raise ValueError(f"{field_name} must be a boolean, got {value!r}")


def _build(merged: dict[str, object], source: Path | None) -> ClientConfig:
    return ClientConfig(
        server_url=str(merged["server_url"]),
        endpoint=str(merged["endpoint"]),
        trigger=str(merged["trigger"]),
        one_key_trigger=str(merged["one_key_trigger"]),
        auth_token=str(merged["auth_token"]),
        input_device=str(merged["input_device"]),
        audio_format=str(merged["audio_format"]).lower(),
        auto_paste=_coerce_bool(merged["auto_paste"], field_name="auto_paste"),
        paste_delay_ms=int(merged["paste_delay_ms"]),
        source_path=source,
    )


def load_config(explicit: Path | None = None) -> ClientConfig:
    """Load a ``ClientConfig`` from the first matching config file, or defaults."""
    data: dict[str, object] = {}
    source: Path | None = None
    for path in _candidate_paths(explicit):
        if path.is_file():
            with path.open("rb") as fh:
                data = tomllib.load(fh)
            source = path
            break

    merged: dict[str, object] = {**DEFAULTS}
    for key in DEFAULTS:
        if key in data and data[key] is not None:
            merged[key] = data[key]

    return _build(merged, source)


def load_from(path: Path) -> ClientConfig:
    """Load a specific file without re-running the candidate-path search.

    Used by the tray watcher to re-validate the active config when its mtime
    advances. ``path`` must exist; errors propagate as ``ValueError`` /
    ``tomllib.TOMLDecodeError`` so the caller can surface them to the user.
    """
    with path.open("rb") as fh:
        data = tomllib.load(fh)
    merged: dict[str, object] = {**DEFAULTS}
    for key in DEFAULTS:
        if key in data and data[key] is not None:
            merged[key] = data[key]
    return _build(merged, path)


# Keys that can be swapped into a running client without a restart.
# Readers re-snapshot the config on every upload/paste, so atomic
# replacement of the holder is enough.
HOT_KEYS: tuple[str, ...] = (
    "server_url",
    "endpoint",
    "auth_token",
    "auto_paste",
    "paste_delay_ms",
)

# Keys whose change requires rebuilding the pynput listener(s) or the
# sounddevice input stream. The tray surfaces a "restart required"
# tooltip when any of these diverge and routes the Restart menu item
# through os.execv.
COLD_KEYS: tuple[str, ...] = (
    "trigger",
    "one_key_trigger",
    "input_device",
    "audio_format",
)


@dataclass(frozen=True)
class ConfigDiff:
    """Result of comparing a freshly-loaded config against the live one."""

    hot_changed: tuple[str, ...] = ()
    cold_changed: tuple[str, ...] = ()

    def is_empty(self) -> bool:
        return not self.hot_changed and not self.cold_changed

    def requires_restart(self) -> bool:
        return bool(self.cold_changed)


def diff(old: ClientConfig, new: ClientConfig) -> ConfigDiff:
    """Classify which keys changed between the live config and a new one.

    ``source_path`` is deliberately ignored: renaming the file on disk is not
    a semantic change for the purposes of reload notifications.
    """
    hot: list[str] = []
    cold: list[str] = []
    for key in HOT_KEYS:
        if getattr(old, key) != getattr(new, key):
            hot.append(key)
    for key in COLD_KEYS:
        if getattr(old, key) != getattr(new, key):
            cold.append(key)
    return ConfigDiff(hot_changed=tuple(hot), cold_changed=tuple(cold))
