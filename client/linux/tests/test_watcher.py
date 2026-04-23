"""Unit tests for ``ghostscribe_client.watcher.poll_once``.

The polling loop itself runs in a daemon thread, but the per-tick logic
lives in :func:`poll_once` which is a pure function over an explicit
state object. Driving it directly keeps the tests deterministic and
avoids the ~1 s sleep overhead of the spawned thread.
"""

from __future__ import annotations

import os
import time
from pathlib import Path

import pytest

from ghostscribe_client import config as _config
from ghostscribe_client import watcher as _watcher


def _baseline(path: Path) -> _config.ClientConfig:
    """Fresh baseline pointing at ``path`` so diff() ignores source_path
    differences and only compares semantic fields."""
    cfg = _config.load_from(path)
    return cfg


def _bump_mtime(path: Path) -> None:
    """Force a strictly-greater mtime, even on filesystems with 1 s
    granularity (older ext4 mounts and many SMB shares)."""
    new = path.stat().st_mtime + 2
    os.utime(path, (new, new))


def _write(path: Path, body: str) -> None:
    path.write_text(body)
    _bump_mtime(path)


@pytest.fixture
def initial(tmp_path: Path) -> Path:
    p = tmp_path / "config.toml"
    p.write_text('server_url = "http://a:5005"\n')
    return p


def test_poll_once_silent_when_nothing_changed(initial: Path) -> None:
    state = _watcher._State(last_mtime=initial.stat().st_mtime)
    events: list[_watcher.WatcherEvent] = []
    keep = _watcher.poll_once(initial, _baseline(initial), state, events.append)
    assert keep is True
    assert events == []


def test_poll_once_emits_reloaded_on_hot_change(initial: Path) -> None:
    base = _baseline(initial)
    state = _watcher._State(last_mtime=initial.stat().st_mtime)
    _write(initial, 'server_url = "http://b:5005"\n')

    events: list[_watcher.WatcherEvent] = []
    _watcher.poll_once(initial, base, state, events.append)

    assert len(events) == 1
    ev = events[0]
    assert isinstance(ev, _watcher.ReloadedEvent)
    assert ev.diff.hot_changed == ("server_url",)
    assert ev.diff.cold_changed == ()
    assert ev.new_config.server_url == "http://b:5005"


def test_poll_once_emits_reloaded_on_cold_change(initial: Path) -> None:
    base = _baseline(initial)
    state = _watcher._State(last_mtime=initial.stat().st_mtime)
    _write(initial, 'trigger = "key:ctrl+m"\n')

    events: list[_watcher.WatcherEvent] = []
    _watcher.poll_once(initial, base, state, events.append)

    assert len(events) == 1
    ev = events[0]
    assert isinstance(ev, _watcher.ReloadedEvent)
    assert "trigger" in ev.diff.cold_changed
    assert ev.diff.requires_restart()


def test_poll_once_silent_when_mtime_changes_but_content_does_not(
    initial: Path,
) -> None:
    """A 'save with no edits' is the most common false-positive source;
    we want zero notifications in that case."""
    base = _baseline(initial)
    state = _watcher._State(last_mtime=initial.stat().st_mtime)
    _bump_mtime(initial)  # Touch only.

    events: list[_watcher.WatcherEvent] = []
    _watcher.poll_once(initial, base, state, events.append)

    assert events == []


def test_poll_once_emits_parse_error_for_broken_toml(initial: Path) -> None:
    base = _baseline(initial)
    state = _watcher._State(last_mtime=initial.stat().st_mtime)
    _write(initial, "this = is = nonsense = toml\n")

    events: list[_watcher.WatcherEvent] = []
    _watcher.poll_once(initial, base, state, events.append)

    assert len(events) == 1
    assert isinstance(events[0], _watcher.ParseErrorEvent)
    assert "TOMLDecodeError" in events[0].message or "Decode" in events[0].message


def test_poll_once_reports_missing_only_once(tmp_path: Path) -> None:
    """If the user moves the file away the watcher should fire once and
    then stay quiet until the file reappears, otherwise the log fills up."""
    p = tmp_path / "absent.toml"
    base = _config.ClientConfig(
        server_url="http://x", endpoint="/v1/auto", trigger="mouse:x2"
    )
    state = _watcher._State()

    events: list[_watcher.WatcherEvent] = []
    _watcher.poll_once(p, base, state, events.append)
    _watcher.poll_once(p, base, state, events.append)

    assert len(events) == 1
    assert isinstance(events[0], _watcher.MissingEvent)


def test_poll_once_resumes_after_file_reappears(tmp_path: Path) -> None:
    p = tmp_path / "comes-back.toml"
    base = _config.ClientConfig(
        server_url="http://x", endpoint="/v1/auto", trigger="mouse:x2"
    )
    state = _watcher._State()

    events: list[_watcher.WatcherEvent] = []
    _watcher.poll_once(p, base, state, events.append)  # missing
    p.write_text('endpoint = "/v1/en"\n')
    _watcher.poll_once(p, base, state, events.append)  # reloaded

    kinds = [type(e).__name__ for e in events]
    assert kinds == ["MissingEvent", "ReloadedEvent"]
    reloaded = events[1]
    assert isinstance(reloaded, _watcher.ReloadedEvent)
    # The brand-new file's endpoint differs from the synthetic baseline;
    # other DEFAULTS-vs-baseline mismatches may also surface, but the key
    # contract is that the explicit edit shows up.
    assert "endpoint" in reloaded.diff.hot_changed
    assert reloaded.new_config.endpoint == "/v1/en"
