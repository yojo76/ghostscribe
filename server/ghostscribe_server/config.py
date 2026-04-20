"""Environment-driven configuration for the GhostScribe server.

All settings are optional; sensible defaults let you run the server with
``uvicorn ghostscribe_server.app:app`` and no extra config.

Overridable via ``GHOSTSCRIBE_*`` environment variables or an
``EnvironmentFile=`` entry in the systemd unit.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path


def _env_str(name: str, default: str) -> str:
    value = os.environ.get(name)
    return value if value not in (None, "") else default


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw in (None, ""):
        return default
    try:
        return int(raw)
    except ValueError as exc:
        raise ValueError(f"{name} must be an integer, got {raw!r}") from exc


def _env_optional_str(name: str) -> str | None:
    value = os.environ.get(name)
    return value if value not in (None, "") else None


@dataclass(frozen=True)
class Settings:
    host: str
    port: int
    model_name: str
    device: str
    compute_type: str
    log_path: Path
    max_upload_mb: int
    auth_token: str | None

    @property
    def max_upload_bytes(self) -> int:
        return self.max_upload_mb * 1024 * 1024

    @property
    def auth_required(self) -> bool:
        return self.auth_token is not None


def load_settings() -> Settings:
    """Build a ``Settings`` instance from the current process environment."""
    log_path_raw = _env_str(
        "GHOSTSCRIBE_LOG_PATH",
        str(Path("logs") / "ghostscribe_server.log"),
    )
    return Settings(
        host=_env_str("GHOSTSCRIBE_HOST", "0.0.0.0"),
        port=_env_int("GHOSTSCRIBE_PORT", 5005),
        model_name=_env_str("GHOSTSCRIBE_MODEL", "large-v3-turbo"),
        device=_env_str("GHOSTSCRIBE_DEVICE", "cuda"),
        compute_type=_env_str("GHOSTSCRIBE_COMPUTE_TYPE", "int8_float16"),
        log_path=Path(log_path_raw),
        max_upload_mb=_env_int("GHOSTSCRIBE_MAX_UPLOAD_MB", 25),
        auth_token=_env_optional_str("GHOSTSCRIBE_AUTH_TOKEN"),
    )
