"""Integration tests for the ``submit()`` HTTP + paste pipeline."""

from __future__ import annotations

from typing import Any

import numpy as np
import pytest

from ghostscribe_client import __main__ as gsclient
from ghostscribe_client.config import ClientConfig


class _FakeResponse:
    def __init__(
        self,
        status_code: int = 200,
        json_body: dict[str, Any] | None = None,
        text: str = "",
    ) -> None:
        self.status_code = status_code
        self._json_body = json_body
        self.text = text or (str(json_body) if json_body is not None else "")

    def json(self) -> dict[str, Any]:
        if self._json_body is None:
            raise ValueError("no json body")
        return self._json_body


class _FakeHttpxClient:
    """Captures the last ``post()`` call and returns a canned response."""

    def __init__(self, response: _FakeResponse) -> None:
        self.response = response
        self.calls: list[dict[str, Any]] = []

    def post(
        self, url: str, *, files: dict[str, Any], headers: dict[str, str], timeout: float
    ) -> _FakeResponse:
        self.calls.append(
            {
                "url": url,
                "files": files,
                "headers": dict(headers),
                "timeout": timeout,
            }
        )
        return self.response


@pytest.fixture
def base_cfg() -> ClientConfig:
    return ClientConfig(
        server_url="http://server:5005",
        endpoint="/v1/en",
        trigger="mouse:x2",
        auth_token="",
        input_device="",
        audio_format="wav",
        auto_paste=True,
        paste_delay_ms=10,
    )


@pytest.fixture
def sample_audio() -> np.ndarray:
    return np.zeros(1600, dtype=np.int16)  # 0.1 s of silence


@pytest.fixture(autouse=True)
def neutralize_io(monkeypatch: pytest.MonkeyPatch) -> None:
    """Stub out subprocess + pynput-backed paste primitives by default."""
    monkeypatch.setattr(gsclient, "inject_paste", lambda *a, **kw: None)
    monkeypatch.setattr(gsclient, "detect_terminal_focus", lambda: False)
    monkeypatch.setattr(gsclient, "read_clipboard", lambda: None)
    monkeypatch.setattr(gsclient, "copy_to_clipboard", lambda _text: True)
    monkeypatch.setattr(gsclient.time, "sleep", lambda _s: None)


def test_submit_no_audio_is_noop(
    base_cfg: ClientConfig, capsys: pytest.CaptureFixture[str]
) -> None:
    http = _FakeHttpxClient(_FakeResponse())
    gsclient.submit(base_cfg, http, None)  # type: ignore[arg-type]
    assert http.calls == []
    assert "no audio captured" in capsys.readouterr().err


def test_submit_happy_path_posts_multipart(
    base_cfg: ClientConfig, sample_audio: np.ndarray
) -> None:
    http = _FakeHttpxClient(
        _FakeResponse(
            json_body={"text": "hello world", "language": "en", "language_probability": 0.99}
        )
    )
    gsclient.submit(base_cfg, http, sample_audio)  # type: ignore[arg-type]

    assert len(http.calls) == 1
    call = http.calls[0]
    assert call["url"] == "http://server:5005/v1/en"
    assert "X-Auth-Token" not in call["headers"]
    assert "audio" in call["files"]
    filename, payload, mime = call["files"]["audio"]
    assert filename == "recording.wav"
    assert mime == "audio/wav"
    assert payload[:4] == b"RIFF"


def test_submit_sends_auth_header_when_configured(
    base_cfg: ClientConfig, sample_audio: np.ndarray
) -> None:
    cfg = ClientConfig(
        server_url=base_cfg.server_url,
        endpoint=base_cfg.endpoint,
        trigger=base_cfg.trigger,
        auth_token="s3cret",
        audio_format=base_cfg.audio_format,
        auto_paste=base_cfg.auto_paste,
        paste_delay_ms=base_cfg.paste_delay_ms,
    )
    http = _FakeHttpxClient(
        _FakeResponse(
            json_body={"text": "t", "language": "en", "language_probability": 0.9}
        )
    )
    gsclient.submit(cfg, http, sample_audio)  # type: ignore[arg-type]
    assert http.calls[0]["headers"]["X-Auth-Token"] == "s3cret"


def test_submit_save_paste_restore_flow(
    base_cfg: ClientConfig,
    sample_audio: np.ndarray,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Clipboard is restored AFTER paste injection."""
    clip_writes: list[str] = []

    monkeypatch.setattr(gsclient, "read_clipboard", lambda: "previous-value")
    monkeypatch.setattr(
        gsclient, "copy_to_clipboard", lambda text: (clip_writes.append(text), True)[1]
    )

    http = _FakeHttpxClient(
        _FakeResponse(
            json_body={"text": "hello", "language": "en", "language_probability": 0.99}
        )
    )
    gsclient.submit(base_cfg, http, sample_audio)  # type: ignore[arg-type]

    # Expect: first write = transcript + trailing space (so back-to-back
    # takes don't concatenate in the target field); second = restored.
    assert clip_writes == ["hello ", "previous-value"]


def test_submit_terminal_focus_triggers_shift(
    base_cfg: ClientConfig,
    sample_audio: np.ndarray,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, Any] = {}

    def fake_inject(delay_ms: int, use_shift: bool = False) -> None:
        captured["use_shift"] = use_shift
        captured["delay_ms"] = delay_ms

    monkeypatch.setattr(gsclient, "detect_terminal_focus", lambda: True)
    monkeypatch.setattr(gsclient, "inject_paste", fake_inject)

    http = _FakeHttpxClient(
        _FakeResponse(
            json_body={"text": "ls -la", "language": "en", "language_probability": 0.9}
        )
    )
    gsclient.submit(base_cfg, http, sample_audio)  # type: ignore[arg-type]
    assert captured["use_shift"] is True


def test_submit_empty_transcript_skips_paste(
    base_cfg: ClientConfig,
    sample_audio: np.ndarray,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    copied: list[str] = []
    monkeypatch.setattr(
        gsclient, "copy_to_clipboard", lambda t: (copied.append(t), True)[1]
    )
    http = _FakeHttpxClient(
        _FakeResponse(
            json_body={"text": "", "language": "en", "language_probability": 0.1}
        )
    )
    gsclient.submit(base_cfg, http, sample_audio)  # type: ignore[arg-type]
    assert copied == []
    assert "empty transcript" in capsys.readouterr().err


def test_submit_http_4xx_logs_and_skips_paste(
    base_cfg: ClientConfig,
    sample_audio: np.ndarray,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    copied: list[str] = []
    monkeypatch.setattr(
        gsclient, "copy_to_clipboard", lambda t: (copied.append(t), True)[1]
    )
    http = _FakeHttpxClient(_FakeResponse(status_code=401, text="nope"))
    gsclient.submit(base_cfg, http, sample_audio)  # type: ignore[arg-type]
    assert copied == []
    assert "HTTP 401" in capsys.readouterr().err


def test_submit_non_json_response_handled(
    base_cfg: ClientConfig,
    sample_audio: np.ndarray,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    copied: list[str] = []
    monkeypatch.setattr(
        gsclient, "copy_to_clipboard", lambda t: (copied.append(t), True)[1]
    )
    http = _FakeHttpxClient(_FakeResponse(status_code=200, text="<html>oops</html>"))
    gsclient.submit(base_cfg, http, sample_audio)  # type: ignore[arg-type]
    assert copied == []
    assert "non-JSON" in capsys.readouterr().err


def test_submit_auto_paste_off_does_not_touch_clipboard(
    base_cfg: ClientConfig,
    sample_audio: np.ndarray,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cfg = ClientConfig(
        server_url=base_cfg.server_url,
        endpoint=base_cfg.endpoint,
        trigger=base_cfg.trigger,
        auth_token="",
        audio_format=base_cfg.audio_format,
        auto_paste=False,
        paste_delay_ms=base_cfg.paste_delay_ms,
    )
    copied: list[str] = []
    monkeypatch.setattr(
        gsclient, "copy_to_clipboard", lambda t: (copied.append(t), True)[1]
    )
    http = _FakeHttpxClient(
        _FakeResponse(
            json_body={"text": "hi", "language": "en", "language_probability": 0.9}
        )
    )
    gsclient.submit(cfg, http, sample_audio)  # type: ignore[arg-type]
    assert copied == []
