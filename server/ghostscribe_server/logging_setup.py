"""Logging configuration for the GhostScribe server.

Installs a dual handler (rotating-ish file + stdout) using ISO-8601
timestamps. The file path is portable: whatever the caller passes in (the
config defaults to ``./logs/ghostscribe_server.log``).
"""

from __future__ import annotations

import logging
import sys
from datetime import datetime
from pathlib import Path


class _ISOFormatter(logging.Formatter):
    def formatTime(self, record: logging.LogRecord, datefmt: str | None = None) -> str:  # noqa: N802
        return datetime.fromtimestamp(record.created).strftime("%Y-%m-%d %H:%M:%S")


def configure_logging(log_path: Path, level: int = logging.INFO) -> logging.Logger:
    """Configure the root logger with file + stdout handlers.

    Creates the parent directory of ``log_path`` if it does not exist. If the
    file cannot be opened (e.g. read-only filesystem), the file handler is
    skipped and a warning is emitted on stdout instead.
    """
    formatter = _ISOFormatter("%(asctime)s [%(levelname)s] %(name)s: %(message)s")

    root = logging.getLogger()
    root.setLevel(level)

    for handler in list(root.handlers):
        root.removeHandler(handler)

    stream_handler = logging.StreamHandler(stream=sys.stdout)
    stream_handler.setFormatter(formatter)
    root.addHandler(stream_handler)

    try:
        log_path.parent.mkdir(parents=True, exist_ok=True)
        file_handler = logging.FileHandler(log_path, encoding="utf-8")
        file_handler.setFormatter(formatter)
        root.addHandler(file_handler)
    except OSError as exc:
        root.warning("Could not open log file %s: %s (file logging disabled)", log_path, exc)

    return logging.getLogger("ghostscribe")
