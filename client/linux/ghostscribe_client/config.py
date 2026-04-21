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


DEFAULTS: dict[str, str] = {
    "server_url": "http://localhost:5005",
    "endpoint": "/v1/auto",
    "ptt_key": "ctrl_r",
    "auth_token": "",
    "input_device": "",
}


@dataclass(frozen=True)
class ClientConfig:
    server_url: str
    endpoint: str
    ptt_key: str
    auth_token: str = ""
    input_device: str = ""
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

    merged: dict[str, str] = {**DEFAULTS}
    for key in DEFAULTS:
        if key in data and data[key] is not None:
            merged[key] = str(data[key])

    return ClientConfig(
        server_url=merged["server_url"],
        endpoint=merged["endpoint"],
        ptt_key=merged["ptt_key"],
        auth_token=merged["auth_token"],
        input_device=merged["input_device"],
        source_path=source,
    )
