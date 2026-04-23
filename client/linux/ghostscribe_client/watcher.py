"""mtime-polling config-file watcher for the Linux client.

Mirrors the Rust client's `watcher.rs`. Rationale for polling instead of
`inotify` / `watchdog`:

1. Editors routinely save via rename-and-replace or atomic-rename-from-
   tempfile. Each editor's event stream looks different; a one-second
   `stat()` is immune to the distinction.
2. This is a single-file watcher. The overhead of a thread waking once
   per second to do one `stat()` is negligible and requires no extra
   dependency.
3. Parse errors belong in the same module as the reparse, so the caller
   sees them as a typed event and can surface them to the user.

Detected transitions (relative to the last observed state):
- file mtime advanced  -> reparse -> `reloaded` / `parse_error`
- file disappeared     -> `missing` (fired once; subsequent polls silent)
- file reappeared      -> treated as an mtime advance on the new file
"""

from __future__ import annotations

import os
import threading
import time
import traceback
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

from . import config as _config
from .config import ClientConfig, ConfigDiff


@dataclass(frozen=True)
class ReloadedEvent:
    """The file parsed cleanly; ``diff`` describes what changed vs baseline."""

    new_config: ClientConfig
    diff: ConfigDiff


@dataclass(frozen=True)
class ParseErrorEvent:
    """The file existed but failed to parse. The live config is untouched."""

    message: str


@dataclass(frozen=True)
class MissingEvent:
    """First-seen 'file gone' transition. Subsequent polls stay silent."""


WatcherEvent = ReloadedEvent | ParseErrorEvent | MissingEvent


@dataclass
class _State:
    """Mutable poll state, kept external to the pure `poll_once` helper
    so tests can drive it deterministically."""

    last_mtime: Optional[float] = None
    last_was_missing: bool = False


def poll_once(
    path: Path,
    baseline: ClientConfig,
    state: _State,
    emit: Callable[[WatcherEvent], bool],
) -> bool:
    """Perform exactly one poll tick.

    Returns ``True`` if the caller should keep polling, ``False`` if the
    sink has gone away (``emit`` returned ``False``) and the watcher
    thread should quit.
    """
    try:
        mtime = os.stat(path).st_mtime
    except FileNotFoundError:
        if not state.last_was_missing:
            state.last_was_missing = True
            state.last_mtime = None
            return emit(MissingEvent())
        return True
    except OSError:
        # Transient errors (EACCES while the editor rewrites the file,
        # for example). Treat like "no change" and try again next tick.
        return True

    state.last_was_missing = False
    if state.last_mtime is not None and mtime == state.last_mtime:
        return True
    state.last_mtime = mtime

    try:
        new_cfg = _config.load_from(path)
    except Exception as exc:  # tomllib.TOMLDecodeError, ValueError, OSError
        msg = f"{type(exc).__name__}: {exc}"
        return emit(ParseErrorEvent(message=msg))

    d = _config.diff(baseline, new_cfg)
    if d.is_empty():
        # No semantic change; typical for "save with no edits" or
        # whitespace-only changes. Stay silent.
        return True
    return emit(ReloadedEvent(new_config=new_cfg, diff=d))


def spawn(
    path: Path,
    baseline_fn: Callable[[], ClientConfig],
    on_event: Callable[[WatcherEvent], None],
    stop_event: threading.Event,
    *,
    interval: float = 1.0,
) -> threading.Thread:
    """Start a daemon thread that polls ``path`` every ``interval`` seconds.

    ``baseline_fn`` is called at every tick so diffs are computed against
    the *latest* live config (otherwise two quick edits would be diffed
    against the same snapshot and the second would look like a revert).
    """

    def _emit(event: WatcherEvent) -> bool:
        try:
            on_event(event)
        except Exception:
            # Never let a callback crash kill the watcher thread; log and
            # keep going. A crashing callback will starve the user of
            # reload events but shouldn't bring down the hotkey loop.
            traceback.print_exc()
        return True

    def _run() -> None:
        st = _State()
        try:
            st.last_mtime = os.stat(path).st_mtime
        except OSError:
            st.last_was_missing = True

        while not stop_event.is_set():
            keep_going = poll_once(path, baseline_fn(), st, _emit)
            if not keep_going:
                return
            stop_event.wait(interval)

    t = threading.Thread(target=_run, name="gs-config-watcher", daemon=True)
    t.start()
    return t
