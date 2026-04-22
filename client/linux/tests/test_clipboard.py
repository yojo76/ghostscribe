"""Unit tests for clipboard/xdotool helpers in ghostscribe_client.__main__."""

from __future__ import annotations

import subprocess
from typing import Any

import pytest

from ghostscribe_client import __main__ as gsclient


class _CompletedProcess:
    def __init__(self, returncode: int, stdout: bytes = b"") -> None:
        self.returncode = returncode
        self.stdout = stdout


# --------------------------------------------------------------------------- #
# read_clipboard                                                              #
# --------------------------------------------------------------------------- #


def test_read_clipboard_returns_none_when_xclip_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(gsclient.shutil, "which", lambda _: None)
    assert gsclient.read_clipboard() is None


def test_read_clipboard_returns_decoded_stdout(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(gsclient.shutil, "which", lambda _: "/usr/bin/xclip")
    monkeypatch.setattr(
        gsclient.subprocess,
        "run",
        lambda *a, **kw: _CompletedProcess(0, stdout="saved text".encode()),
    )
    assert gsclient.read_clipboard() == "saved text"


def test_read_clipboard_returns_none_on_nonzero_exit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(gsclient.shutil, "which", lambda _: "/usr/bin/xclip")
    monkeypatch.setattr(
        gsclient.subprocess,
        "run",
        lambda *a, **kw: _CompletedProcess(1),
    )
    assert gsclient.read_clipboard() is None


def test_read_clipboard_returns_none_on_subprocess_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(gsclient.shutil, "which", lambda _: "/usr/bin/xclip")

    def _boom(*_a: Any, **_kw: Any) -> None:
        raise subprocess.TimeoutExpired(cmd="xclip", timeout=5)

    monkeypatch.setattr(gsclient.subprocess, "run", _boom)
    assert gsclient.read_clipboard() is None


# --------------------------------------------------------------------------- #
# copy_to_clipboard                                                           #
# --------------------------------------------------------------------------- #


def test_copy_to_clipboard_returns_false_when_xclip_missing(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(gsclient.shutil, "which", lambda _: None)
    assert gsclient.copy_to_clipboard("x") is False
    err = capsys.readouterr().err
    assert "xclip not found" in err


def test_copy_to_clipboard_success(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, Any] = {}

    def _fake_run(cmd: list[str], *, input: bytes, check: bool, timeout: int) -> None:
        captured["cmd"] = cmd
        captured["input"] = input

    monkeypatch.setattr(gsclient.shutil, "which", lambda _: "/usr/bin/xclip")
    monkeypatch.setattr(gsclient.subprocess, "run", _fake_run)

    assert gsclient.copy_to_clipboard("hello") is True
    assert captured["cmd"][0] == "/usr/bin/xclip"
    assert captured["input"] == b"hello"


def test_copy_to_clipboard_failure(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    def _boom(*_a: Any, **_kw: Any) -> None:
        raise subprocess.CalledProcessError(returncode=1, cmd="xclip")

    monkeypatch.setattr(gsclient.shutil, "which", lambda _: "/usr/bin/xclip")
    monkeypatch.setattr(gsclient.subprocess, "run", _boom)

    assert gsclient.copy_to_clipboard("x") is False
    assert "xclip failed" in capsys.readouterr().err


# --------------------------------------------------------------------------- #
# detect_terminal_focus                                                       #
# --------------------------------------------------------------------------- #


def test_detect_terminal_focus_missing_xdotool(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(gsclient.shutil, "which", lambda _: None)
    assert gsclient.detect_terminal_focus() is False


@pytest.mark.parametrize(
    "classname,expected",
    [
        ("Gnome-terminal", True),
        ("gnome-terminal-server", True),
        ("Alacritty", True),
        ("alacritty", True),
        ("kitty", True),
        ("firefox", False),
        ("thunderbird", False),
        ("Code", False),
        ("", False),
    ],
)
def test_detect_terminal_focus_by_class(
    monkeypatch: pytest.MonkeyPatch, classname: str, expected: bool
) -> None:
    monkeypatch.setattr(gsclient.shutil, "which", lambda _: "/usr/bin/xdotool")
    monkeypatch.setattr(
        gsclient.subprocess,
        "run",
        lambda *a, **kw: _CompletedProcess(0, stdout=classname.encode()),
    )
    assert gsclient.detect_terminal_focus() is expected


def test_detect_terminal_focus_nonzero_exit_treated_as_non_terminal(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(gsclient.shutil, "which", lambda _: "/usr/bin/xdotool")
    monkeypatch.setattr(
        gsclient.subprocess,
        "run",
        lambda *a, **kw: _CompletedProcess(1, stdout=b"Alacritty"),
    )
    assert gsclient.detect_terminal_focus() is False


def test_detect_terminal_focus_subprocess_error_returns_false(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(gsclient.shutil, "which", lambda _: "/usr/bin/xdotool")

    def _boom(*_a: Any, **_kw: Any) -> None:
        raise subprocess.TimeoutExpired(cmd="xdotool", timeout=2)

    monkeypatch.setattr(gsclient.subprocess, "run", _boom)
    assert gsclient.detect_terminal_focus() is False
