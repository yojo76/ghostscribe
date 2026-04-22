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
