"""Shared pytest fixtures for the GhostScribe Linux client tests."""

from __future__ import annotations

from pathlib import Path

import pytest

SAMPLE_TOML = """\
server_url      = "http://example.internal:5005"
endpoint        = "/v1/en"
trigger         = "key:ctrl+shift+g"
one_key_trigger = "key:ctrl"
auth_token      = "s3cret"
input_device    = "USB Audio Device"
audio_format    = "wav"
auto_paste      = false
paste_delay_ms  = 120
"""


@pytest.fixture
def sample_config_path(tmp_path: Path) -> Path:
    p = tmp_path / "config.toml"
    p.write_text(SAMPLE_TOML)
    return p
