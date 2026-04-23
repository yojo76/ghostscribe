"""GhostScribe minimal PTT client.

Hold the configured trigger (a mouse button or keyboard combo); while it's
held, 16 kHz mono 16-bit audio is captured. On release the buffer is
encoded (FLAC by default), POSTed to the configured endpoint, and the
returned transcript is pushed to the X11 CLIPBOARD via ``xclip``.

By default the client runs in tray mode: a colour-coded system-tray icon
with a right-click menu, live config reload, and a log file at
~/.local/state/ghostscribe/ghostscribe.log. Pass --no-tray to run in
command-line mode with output going to stderr only.

Usage:
    python -m ghostscribe_client                      # tray mode (default)
    python -m ghostscribe_client --no-tray            # command-line mode
    python -m ghostscribe_client --config ~/my_gs.toml
    python -m ghostscribe_client --trigger key:ctrl+g
"""

from __future__ import annotations

import argparse
import io
import os
import queue
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

import numpy as np
import sounddevice as sd
import soundfile as sf
import httpx
from pynput import keyboard, mouse

from . import config as _config
from . import tray as _tray
from . import watcher as _watcher
from .config import ClientConfig, load_config
from .tray import MenuAction, TrayState

SAMPLE_RATE = 16_000
CHANNELS = 1
DTYPE = "int16"


# --------------------------------------------------------------------------- #
# Helpers                                                                     #
# --------------------------------------------------------------------------- #


def _eprint(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


class _TeeStream:
    """Writes to both the original stderr and an open log file.

    Set ``paused = True`` to stop writing to the log file without
    uninstalling the tee. stderr always gets every line regardless.
    """

    def __init__(self, original, log_fh, paused: bool = False):
        self._orig = original
        self._log = log_fh
        self.paused = paused

    def write(self, data: str) -> int:
        self._orig.write(data)
        self._orig.flush()
        if not self.paused:
            self._log.write(data)
            self._log.flush()
        return len(data)

    def flush(self) -> None:
        self._orig.flush()
        if not self.paused:
            self._log.flush()

    def fileno(self):
        return self._orig.fileno()

    def __getattr__(self, name):
        return getattr(self._orig, name)


def _setup_tray_log() -> "IO[str] | None":
    """Install a tee on sys.stderr that can write to the log file on demand.

    Starts paused (logging off). The caller enables logging by setting
    ``sys.stderr.paused = False``. Returns the open file handle so the
    caller can close it on exit, or None if the file could not be created.
    """
    lp = _log_file_path()
    try:
        lp.parent.mkdir(parents=True, exist_ok=True)
        fh = lp.open("a", encoding="utf-8")
        sys.stderr = _TeeStream(sys.stderr, fh, paused=True)  # type: ignore[assignment]
        return fh
    except OSError:
        return None


def _resolve_input_device(raw: str) -> int | str | None:
    raw = raw.strip()
    if not raw:
        return None
    if raw.lstrip("-").isdigit():
        return int(raw)
    return raw


# --------------------------------------------------------------------------- #
# Trigger parsing                                                             #
# --------------------------------------------------------------------------- #


# pynput exposes a different set of Button members per platform: Linux has
# ``button8``..``button30``, Windows has ``x1``/``x2``. Accept common aliases
# and resolve them with getattr so a config like ``mouse:x2`` works on both.
_MOUSE_BUTTON_ALIASES: dict[str, tuple[str, ...]] = {
    "left": ("left",),
    "middle": ("middle",),
    "right": ("right",),
    "x1": ("x1", "button8"),
    "x2": ("x2", "button9"),
    "back": ("button8", "x1"),
    "forward": ("button9", "x2"),
    "button8": ("button8",),
    "button9": ("button9",),
}


def _resolve_mouse_button(name: str) -> mouse.Button | None:
    for cand in _MOUSE_BUTTON_ALIASES.get(name.lower(), (name.lower(),)):
        btn = getattr(mouse.Button, cand, None)
        if isinstance(btn, mouse.Button):
            return btn
    return None

# Modifier families: each name maps to every pynput Key that should
# satisfy it. "ctrl" matches ctrl_l, ctrl_r, and the bare Key.ctrl.
_MODIFIER_FAMILIES: dict[str, frozenset[Any]] = {
    "ctrl": frozenset(
        {keyboard.Key.ctrl, keyboard.Key.ctrl_l, keyboard.Key.ctrl_r}
    ),
    "shift": frozenset(
        {keyboard.Key.shift, keyboard.Key.shift_l, keyboard.Key.shift_r}
    ),
    "alt": frozenset(
        {
            keyboard.Key.alt,
            keyboard.Key.alt_l,
            keyboard.Key.alt_r,
            getattr(keyboard.Key, "alt_gr", keyboard.Key.alt_r),
        }
    ),
    "super": frozenset(
        {keyboard.Key.cmd, keyboard.Key.cmd_l, keyboard.Key.cmd_r}
    ),
}


@dataclass(frozen=True)
class KeyTrigger:
    modifiers: tuple[frozenset[Any], ...]
    target: Any  # keyboard.Key or keyboard.KeyCode
    label: str


@dataclass(frozen=True)
class MouseTrigger:
    button: mouse.Button
    label: str


Trigger = KeyTrigger | MouseTrigger


@dataclass(frozen=True)
class OneKeyLinuxTrigger:
    """Single-key PTT trigger. Restricted to modifier families and F-keys."""
    key_family: frozenset[Any]
    label: str


def parse_one_key_trigger(spec: str) -> OneKeyLinuxTrigger | None:
    """Return an `OneKeyLinuxTrigger`, or `None` when spec is empty/disabled."""
    s = spec.strip()
    if not s:
        return None
    lower = s.lower()
    if not lower.startswith("key:"):
        raise ValueError(
            f"one_key_trigger must start with 'key:' (got {s!r})"
        )
    rest = lower[4:]
    if "+" in rest:
        raise ValueError(
            f"one_key_trigger cannot be a chord (got {s!r}); use trigger= for chords"
        )
    # Intentionally restricted: ctrl and alt only from modifier families.
    # shift triggers on every capital letter; super conflicts with WM shortcuts.
    if rest == "ctrl":
        return OneKeyLinuxTrigger(key_family=_MODIFIER_FAMILIES["ctrl"], label=lower)
    if rest == "alt":
        return OneKeyLinuxTrigger(key_family=_MODIFIER_FAMILIES["alt"], label=lower)
    if rest.startswith("f") and rest[1:].isdigit():
        n = int(rest[1:])
        if 1 <= n <= 24:
            fkey = getattr(keyboard.Key, f"f{n}", None)
            if fkey is not None:
                return OneKeyLinuxTrigger(key_family=frozenset({fkey}), label=lower)
    raise ValueError(
        f"one_key_trigger must be one of: key:ctrl, key:alt, key:f1..key:f24 "
        f"(got {s!r}). Letters, digits, shift, and super are intentionally rejected."
    )


def _resolve_target_key(name: str) -> Any:
    if not name:
        raise ValueError("empty key name")
    special = getattr(keyboard.Key, name, None)
    if isinstance(special, keyboard.Key):
        return special
    if len(name) == 1:
        return keyboard.KeyCode.from_char(name)
    raise ValueError(
        f"unknown key {name!r}. Use a pynput Key name (ctrl_r, f12, ...) "
        "or a single character."
    )


def parse_trigger(spec: str) -> Trigger:
    spec = spec.strip()
    if ":" not in spec:
        raise ValueError(
            f"invalid trigger {spec!r}; expected 'mouse:<button>' or 'key:<combo>'"
        )
    kind, _, value = spec.partition(":")
    kind = kind.strip().lower()
    value = value.strip()

    if kind == "mouse":
        btn = _resolve_mouse_button(value)
        if btn is None:
            raise ValueError(
                f"unknown mouse button {value!r}. "
                f"Known aliases: {', '.join(sorted(_MOUSE_BUTTON_ALIASES))}"
            )
        return MouseTrigger(button=btn, label=f"mouse:{value.lower()}")

    if kind == "key":
        parts = [p.strip() for p in value.split("+")]
        if not parts or not all(parts):
            raise ValueError(f"invalid key trigger {value!r}")
        *mod_names, target_name = parts
        mods: list[frozenset[Any]] = []
        for name in mod_names:
            fam = _MODIFIER_FAMILIES.get(name.lower())
            if fam is None:
                raise ValueError(
                    f"unknown modifier {name!r}. "
                    f"Known: {', '.join(sorted(_MODIFIER_FAMILIES))}"
                )
            mods.append(fam)
        target = _resolve_target_key(target_name)
        label = "key:" + "+".join([*(m.lower() for m in mod_names), target_name])
        return KeyTrigger(modifiers=tuple(mods), target=target, label=label)

    raise ValueError(f"unknown trigger kind {kind!r}; expected 'mouse' or 'key'")


# --------------------------------------------------------------------------- #
# Recorder                                                                    #
# --------------------------------------------------------------------------- #


class Recorder:
    """Captures audio into an in-memory list while ``active`` is set."""

    def __init__(self, device: int | str | None) -> None:
        self._device = device
        self._chunks: list[np.ndarray] = []
        self._lock = threading.Lock()
        self._active = threading.Event()
        self._stream: sd.InputStream | None = None

    def start_stream(self) -> None:
        self._stream = sd.InputStream(
            samplerate=SAMPLE_RATE,
            channels=CHANNELS,
            dtype=DTYPE,
            device=self._device,
            callback=self._callback,
            blocksize=0,
        )
        self._stream.start()

    def stop_stream(self) -> None:
        if self._stream is not None:
            self._stream.stop()
            self._stream.close()
            self._stream = None

    def _callback(self, indata, _frames, _time, status) -> None:  # noqa: ANN001
        if status:
            _eprint(f"[audio] {status}")
        if self._active.is_set():
            with self._lock:
                self._chunks.append(indata.copy())

    def begin(self) -> None:
        with self._lock:
            self._chunks.clear()
        self._active.set()

    def end(self) -> np.ndarray | None:
        """Stop capturing and return the collected audio as a single ndarray."""
        self._active.clear()
        with self._lock:
            chunks = self._chunks
            self._chunks = []
        if not chunks:
            return None
        return np.concatenate(chunks, axis=0)

    def cancel(self) -> None:
        """Discard the current take without returning audio."""
        self._active.clear()
        with self._lock:
            self._chunks.clear()

    def checkpoint(self) -> np.ndarray | None:
        """Drain and return audio collected so far without stopping the stream."""
        with self._lock:
            if not self._chunks:
                return None
            audio = np.concatenate(self._chunks, axis=0)
            self._chunks = []
        return audio


_CHUNK_INTERVAL = 2 * 60.0  # auto-send every 2 minutes while recording


class _ChunkTimer:
    """Calls *on_chunk* every _CHUNK_INTERVAL seconds until stopped."""

    def __init__(self, on_chunk: Callable[[], None]) -> None:
        self._on_chunk = on_chunk
        self._timer: threading.Timer | None = None
        self._active = False

    def start(self) -> None:
        self._active = True
        self._arm()

    def stop(self) -> None:
        self._active = False
        if self._timer is not None:
            self._timer.cancel()
            self._timer = None

    def _arm(self) -> None:
        t = threading.Timer(_CHUNK_INTERVAL, self._fire)
        t.daemon = True
        t.start()
        self._timer = t

    def _fire(self) -> None:
        if not self._active:
            return
        self._on_chunk()
        if self._active:
            self._arm()


# --------------------------------------------------------------------------- #
# Encoding                                                                    #
# --------------------------------------------------------------------------- #


def encode_audio(
    audio: np.ndarray, fmt: str
) -> tuple[bytes, str, str]:
    """Encode ``audio`` as FLAC or WAV. Returns (bytes, filename, mime)."""
    buf = io.BytesIO()
    if fmt == "flac":
        sf.write(buf, audio, SAMPLE_RATE, format="FLAC")
        return buf.getvalue(), "recording.flac", "audio/flac"
    if fmt == "wav":
        sf.write(buf, audio, SAMPLE_RATE, subtype="PCM_16", format="WAV")
        return buf.getvalue(), "recording.wav", "audio/wav"
    raise ValueError(f"unknown audio_format {fmt!r}; use 'flac' or 'wav'")


# --------------------------------------------------------------------------- #
# Clipboard                                                                   #
# --------------------------------------------------------------------------- #


def read_clipboard() -> str | None:
    """Return current X11 CLIPBOARD contents, or None if unavailable/empty."""
    xclip = shutil.which("xclip")
    if xclip is None:
        return None
    try:
        result = subprocess.run(
            [xclip, "-selection", "clipboard", "-o"],
            capture_output=True,
            timeout=5,
        )
        if result.returncode == 0:
            return result.stdout.decode("utf-8", errors="replace")
        return None
    except subprocess.SubprocessError:
        return None


def copy_to_clipboard(text: str) -> bool:
    """Push ``text`` onto the X11 CLIPBOARD via ``xclip``. Returns True on success."""
    xclip = shutil.which("xclip")
    if xclip is None:
        _eprint("[paste] xclip not found; install with: sudo apt install xclip")
        return False
    try:
        subprocess.run(
            [xclip, "-selection", "clipboard"],
            input=text.encode("utf-8"),
            check=True,
            timeout=5,
        )
        return True
    except subprocess.SubprocessError as exc:
        _eprint(f"[paste] xclip failed: {exc}")
        return False


_paste_kb: keyboard.Controller | None = None


# Class names returned by ``xdotool getactivewindow getwindowclassname`` for
# terminal emulators that ignore plain Ctrl+V and want Ctrl+Shift+V instead.
# Compared case-insensitively. Add aggressively; a false positive only means
# the user gets Ctrl+Shift+V in a non-terminal, which all major editors also
# accept as paste.
_TERMINAL_CLASSES: frozenset[str] = frozenset(
    s.lower()
    for s in (
        # GNOME / Mate / Cinnamon
        "gnome-terminal-server",
        "Gnome-terminal",
        "mate-terminal",
        "Mate-terminal",
        "Tilix",
        "tilix",
        "Terminator",
        "terminator",
        # KDE
        "konsole",
        "Konsole",
        "yakuake",
        "Yakuake",
        # X classics
        "xterm",
        "XTerm",
        "UXTerm",
        "URxvt",
        "urxvt",
        "rxvt",
        "Rxvt",
        # Modern GPU terminals
        "Alacritty",
        "alacritty",
        "kitty",
        "Kitty",
        "wezterm",
        "WezTerm",
        "org.wezfurlong.wezterm",
        "foot",
        # Cygwin/MSYS-style on Linux
        "mintty",
    )
)


def detect_terminal_focus() -> tuple[bool, str]:
    """Return (is_terminal, window_class) for the X11 foreground window.

    Uses xdotool to get the active window ID then xprop to read WM_CLASS.
    xdotool's getwindowclassname subcommand does not exist in all versions,
    so we use xprop which is universally available on X11.
    Returns (False, "") if either tool is missing or the call fails.
    """
    xdotool = shutil.which("xdotool")
    xprop = shutil.which("xprop")
    if xdotool is None or xprop is None:
        return False, ""
    try:
        win_result = subprocess.run(
            [xdotool, "getactivewindow"],
            capture_output=True,
            timeout=2,
        )
    except subprocess.SubprocessError:
        return False, ""
    if win_result.returncode != 0:
        return False, ""
    win_id = win_result.stdout.decode("utf-8", errors="replace").strip()
    try:
        prop_result = subprocess.run(
            [xprop, "-id", win_id, "WM_CLASS"],
            capture_output=True,
            timeout=2,
        )
    except subprocess.SubprocessError:
        return False, ""
    if prop_result.returncode != 0:
        return False, ""
    # Output format: WM_CLASS(STRING) = "instance", "ClassName"
    # Check both instance and class name against the known-terminal set.
    raw = prop_result.stdout.decode("utf-8", errors="replace")
    names = [s.strip().strip('"') for s in raw.split("=", 1)[-1].split(",")]
    cls = ", ".join(names)
    return any(n.lower() in _TERMINAL_CLASSES for n in names), cls


def inject_enter() -> None:
    """Simulate a single Enter key press in the currently focused window."""
    global _paste_kb
    if _paste_kb is None:
        _paste_kb = keyboard.Controller()
    _paste_kb.press(keyboard.Key.enter)
    _paste_kb.release(keyboard.Key.enter)


_DO_IT_NOW_RE = re.compile(r"^do\s+it\s+now\W*$", re.IGNORECASE)


def _is_do_it_now(text: str) -> bool:
    return bool(_DO_IT_NOW_RE.match(text.strip()))


def inject_paste(delay_ms: int, use_shift: bool = False) -> None:
    """Simulate Ctrl+V (or Ctrl+Shift+V) in the currently focused window.

    Most terminal emulators (GNOME Terminal, Konsole, xterm, kitty, alacritty,
    wezterm, ...) intentionally ignore plain Ctrl+V because Ctrl+V is a valid
    line-discipline character (literal-next, ``^V``); they bind paste to
    Ctrl+Shift+V instead. Pass ``use_shift=True`` for those targets.
    """
    global _paste_kb
    if _paste_kb is None:
        _paste_kb = keyboard.Controller()
    if delay_ms > 0:
        time.sleep(delay_ms / 1000.0)
    _paste_kb.press(keyboard.Key.ctrl)
    if use_shift:
        _paste_kb.press(keyboard.Key.shift)
    try:
        _paste_kb.press("v")
        _paste_kb.release("v")
    finally:
        if use_shift:
            _paste_kb.release(keyboard.Key.shift)
        _paste_kb.release(keyboard.Key.ctrl)


# --------------------------------------------------------------------------- #
# HTTP submission                                                             #
# --------------------------------------------------------------------------- #


def submit(
    cfg: ClientConfig, client: httpx.Client, audio: np.ndarray | None
) -> str | None:
    """Encode + POST + paste the given buffer. Returns the transcript text
    on HTTP success (possibly empty string for "no speech detected"), or
    ``None`` on any failure (network, encoding, HTTP >= 400, non-JSON)."""
    if audio is None or len(audio) == 0:
        _eprint("[send] skipped: no audio captured")
        return None

    try:
        payload, filename, mime = encode_audio(audio, cfg.audio_format)
    except Exception as exc:
        _eprint(f"[send] encoding failed: {exc}")
        return None

    headers: dict[str, str] = {}
    if cfg.has_auth:
        headers["X-Auth-Token"] = cfg.auth_token

    files = {"audio": (filename, payload, mime)}
    size_kb = len(payload) / 1024
    t0 = time.perf_counter()
    try:
        resp = client.post(cfg.url, files=files, headers=headers, timeout=30.0)
    except httpx.HTTPError as exc:
        _eprint(f"[send] failed: {exc}")
        return None
    dt_ms = (time.perf_counter() - t0) * 1000

    if resp.status_code >= 400:
        _eprint(f"[send] HTTP {resp.status_code}: {resp.text.strip()}")
        return None
    try:
        data = resp.json()
    except ValueError:
        _eprint(f"[send] non-JSON response: {resp.text[:200]}")
        return None

    text = (data.get("text") or "").strip()
    lang = data.get("language", "?")
    prob = data.get("language_probability", 0)
    _eprint(f"[recv] {size_kb:.0f} kB in {dt_ms:.0f} ms (lang={lang} p={prob})")
    if not text:
        _eprint("[recv] empty transcript")
        return ""

    if cfg.auto_paste and _is_do_it_now(text):
        _eprint("[do-it-now] Enter")
        inject_enter()
        return text

    pasted = False
    combo_used = "ctrl+v"
    if cfg.auto_paste:
        saved = read_clipboard()
        # Trailing space so back-to-back takes don't concatenate in the
        # target field. Only applied to the pasted copy — not to _eprint().
        if copy_to_clipboard(text + " "):
            use_shift, win_cls = detect_terminal_focus()
            combo_used = "ctrl+shift+v" if use_shift else "ctrl+v"
            _eprint(f"[paste] window={win_cls!r} -> {combo_used}")
            try:
                inject_paste(cfg.paste_delay_ms, use_shift=use_shift)
                pasted = True
                time.sleep(cfg.paste_delay_ms / 1000.0)
                if saved is not None:
                    copy_to_clipboard(saved)
                    _eprint("[paste] clipboard restored")
            except Exception as exc:
                _eprint(f"[paste] {combo_used} injection failed: {exc}")
    if pasted:
        _eprint(f"[paste] pasted via {combo_used} into focused window:")
    else:
        _eprint("[recv] transcript:")
    _eprint(text)
    return text


# --------------------------------------------------------------------------- #
# Main loop                                                                   #
# --------------------------------------------------------------------------- #


def run(cfg: ClientConfig) -> int:
    try:
        trig = parse_trigger(cfg.trigger)
    except ValueError as exc:
        _eprint(f"[fatal] {exc}")
        return 2

    try:
        one_key = parse_one_key_trigger(cfg.one_key_trigger)
    except ValueError as exc:
        _eprint(f"[fatal] {exc}")
        return 2

    device = _resolve_input_device(cfg.input_device)

    _eprint(f"GhostScribe client -> {cfg.url}")
    if cfg.source_path is not None:
        _eprint(f"config:   {cfg.source_path}")
    else:
        _eprint("config:   (defaults, no config file found)")
    _eprint(f"trigger:  {trig.label}")
    _eprint(f"one_key:  {one_key.label if one_key else 'off'}")
    _eprint(f"device:   {device if device is not None else '(system default)'}")
    _eprint(f"format:   {cfg.audio_format}")
    _eprint(
        f"paste:    {'on' if cfg.auto_paste else 'off'}"
        f" (delay {cfg.paste_delay_ms} ms)"
    )
    _eprint(f"auth:     {'on' if cfg.has_auth else 'off'}")
    _eprint("Hold the trigger and speak. Release to transcribe. Ctrl+C to quit.")

    if cfg.auto_paste and shutil.which("xclip") is None:
        _eprint("[warn] auto_paste is on but xclip is not installed.")
    if cfg.auto_paste and (shutil.which("xdotool") is None or shutil.which("xprop") is None):
        _eprint(
            "[warn] xdotool or xprop not found; terminal-focused windows will receive "
            "Ctrl+V instead of Ctrl+Shift+V. Install with: sudo apt install xdotool x11-utils"
        )

    recorder = Recorder(device)
    try:
        recorder.start_stream()
    except sd.PortAudioError as exc:
        _eprint(f"[fatal] could not open audio input: {exc}")
        return 1

    jobs: queue.Queue[np.ndarray | None] = queue.Queue()
    stop = threading.Event()
    recording = threading.Event()

    with httpx.Client() as http:

        def worker() -> None:
            while True:
                item = jobs.get()
                if item is None:
                    return
                try:
                    submit(cfg, http, item)
                except Exception as exc:  # last-ditch
                    _eprint(f"[send] unexpected error: {exc}")

        worker_thread = threading.Thread(target=worker, name="gs-submit", daemon=True)
        worker_thread.start()

        def _on_auto_chunk() -> None:
            audio = recorder.checkpoint()
            if audio is not None and len(audio) > 0:
                _eprint(f"[rec] auto-chunk, {len(audio) * 2 / 1024:.0f} kB")
                jobs.put(audio)

        _chunk_timer = _ChunkTimer(_on_auto_chunk)

        def start_recording() -> None:
            if recording.is_set():
                return
            recording.set()
            recorder.begin()
            _eprint("[rec] ...")
            _chunk_timer.start()

        def stop_recording() -> None:
            _chunk_timer.stop()
            if not recording.is_set():
                return
            recording.clear()
            audio = recorder.end()
            n = 0 if audio is None else len(audio)
            _eprint(f"[rec] stopped, {n * 2 / 1024:.0f} kB raw")
            jobs.put(audio)

        def _cancel_recording() -> None:
            _chunk_timer.stop()
            if not recording.is_set():
                return
            recording.clear()
            recorder.cancel()
            _eprint("[rec] cancelled")

        listeners: list[Any] = []

        # State machine shared across all listeners:
        #   "idle"    – nothing active
        #   "chord"   – recording started by the chord/mouse trigger
        #   "one_key" – recording started by one_key_trigger; cancellable
        #   "lockout" – one_key take cancelled; wait for one-key release
        _mode = "idle"
        _mode_lock = threading.Lock()

        if isinstance(trig, MouseTrigger):
            target_button = trig.button

            def on_click(
                _x: int, _y: int, button: mouse.Button, pressed: bool
            ) -> None:
                nonlocal _mode
                if button != target_button:
                    return
                with _mode_lock:
                    if pressed and _mode == "idle":
                        _mode = "chord"
                        start_recording()
                    elif not pressed and _mode == "chord":
                        _mode = "idle"
                        stop_recording()

            listeners.append(mouse.Listener(on_click=on_click))

        else:  # KeyTrigger
            held: set[Any] = set()
            key_trig: KeyTrigger = trig

            def _chord_involved(key: Any) -> bool:
                if key == key_trig.target:
                    return True
                for fam in key_trig.modifiers:
                    if key in fam:
                        return True
                return False

            def _combo_satisfied() -> bool:
                if key_trig.target not in held:
                    return False
                for fam in key_trig.modifiers:
                    if fam.isdisjoint(held):
                        return False
                return True

            kb_listener: keyboard.Listener | None = None

            def on_press(key: Any) -> None:
                nonlocal _mode
                k = kb_listener.canonical(key) if kb_listener else key
                with _mode_lock:
                    held.add(k)
                    if _mode == "idle":
                        if _combo_satisfied():
                            _mode = "chord"
                            start_recording()
                        elif one_key is not None and k in one_key.key_family:
                            _mode = "one_key"
                            start_recording()
                    elif _mode == "one_key":
                        # Neutral: the one-key itself, and any key that is
                        # part of the configured chord.
                        if k not in one_key.key_family and not _chord_involved(k):
                            _mode = "lockout"
                            _cancel_recording()

            def on_release(key: Any) -> None:
                nonlocal _mode
                k = kb_listener.canonical(key) if kb_listener else key
                with _mode_lock:
                    held.discard(k)
                    if _mode == "chord" and _chord_involved(k):
                        _mode = "idle"
                        stop_recording()
                    elif _mode == "one_key" and one_key is not None and k in one_key.key_family:
                        _mode = "idle"
                        stop_recording()
                    elif _mode == "lockout" and one_key is not None and k in one_key.key_family:
                        _mode = "idle"

            kb_listener = keyboard.Listener(on_press=on_press, on_release=on_release)
            listeners.append(kb_listener)

        # For mouse-chord users who also want one_key, attach a separate
        # keyboard listener to handle the one-key state machine.
        if isinstance(trig, MouseTrigger) and one_key is not None:
            ok_listener: keyboard.Listener | None = None

            def on_ok_press(key: Any) -> None:
                nonlocal _mode
                k = ok_listener.canonical(key) if ok_listener else key
                with _mode_lock:
                    if _mode == "idle" and k in one_key.key_family:
                        _mode = "one_key"
                        start_recording()
                    elif _mode == "one_key" and k not in one_key.key_family:
                        _mode = "lockout"
                        _cancel_recording()

            def on_ok_release(key: Any) -> None:
                nonlocal _mode
                k = ok_listener.canonical(key) if ok_listener else key
                with _mode_lock:
                    if _mode == "one_key" and k in one_key.key_family:
                        _mode = "idle"
                        stop_recording()
                    elif _mode == "lockout" and k in one_key.key_family:
                        _mode = "idle"

            ok_listener = keyboard.Listener(on_press=on_ok_press, on_release=on_ok_release)
            listeners.append(ok_listener)

        for ls in listeners:
            ls.start()

        def _stop(_sig=None, _frame=None) -> None:  # noqa: ANN001
            stop.set()

        signal.signal(signal.SIGINT, _stop)
        try:
            signal.signal(signal.SIGTERM, _stop)
        except (AttributeError, ValueError):
            pass

        try:
            while not stop.is_set():
                stop.wait(0.25)
        finally:
            _eprint("Shutting down...")
            for ls in listeners:
                ls.stop()
            jobs.put(None)
            worker_thread.join(timeout=5.0)
            recorder.stop_stream()

    return 0


# --------------------------------------------------------------------------- #
# Tray mode                                                                   #
# --------------------------------------------------------------------------- #


def _print_banner(cfg: ClientConfig, mode: str) -> None:
    _eprint(f"GhostScribe client ({mode}) -> {cfg.url}")
    if cfg.source_path is not None:
        _eprint(f"config:   {cfg.source_path}")
    else:
        _eprint("config:   (defaults, no config file found)")
    _eprint(f"trigger:  {cfg.trigger}")
    _eprint(
        "one_key:  "
        + (cfg.one_key_trigger if cfg.one_key_trigger else "off")
    )
    _eprint(f"format:   {cfg.audio_format}")
    _eprint(f"auth:     {'on' if cfg.has_auth else 'off'}")
    _eprint(
        f"paste:    {'on' if cfg.auto_paste else 'off'}"
        f" (delay {cfg.paste_delay_ms} ms)"
    )


def _build_listeners(
    trig: Trigger,
    one_key: OneKeyLinuxTrigger | None,
    start_recording: Callable[[], None],
    stop_recording: Callable[[], None],
    cancel_recording: Callable[[], None],
) -> list[Any]:
    """Build the pynput listeners that map trigger events onto record/stop/cancel.

    Matches the state machine in :func:`run`: ``idle`` -> ``chord`` |
    ``one_key`` -> ``idle`` | ``lockout``. Kept as a free function so
    both :func:`run` and :func:`run_tray` can reuse it verbatim.
    """
    listeners: list[Any] = []
    _mode = "idle"
    _mode_lock = threading.Lock()

    if isinstance(trig, MouseTrigger):
        target_button = trig.button

        def on_click(_x: int, _y: int, button: mouse.Button, pressed: bool) -> None:
            nonlocal _mode
            if button != target_button:
                return
            with _mode_lock:
                if pressed and _mode == "idle":
                    _mode = "chord"
                    start_recording()
                elif not pressed and _mode == "chord":
                    _mode = "idle"
                    stop_recording()

        listeners.append(mouse.Listener(on_click=on_click))

    else:
        held: set[Any] = set()
        key_trig: KeyTrigger = trig

        def _chord_involved(key: Any) -> bool:
            if key == key_trig.target:
                return True
            return any(key in fam for fam in key_trig.modifiers)

        def _combo_satisfied() -> bool:
            if key_trig.target not in held:
                return False
            return all(not fam.isdisjoint(held) for fam in key_trig.modifiers)

        kb_listener: keyboard.Listener | None = None

        def on_press(key: Any) -> None:
            nonlocal _mode
            k = kb_listener.canonical(key) if kb_listener else key
            with _mode_lock:
                held.add(k)
                if _mode == "idle":
                    if _combo_satisfied():
                        _mode = "chord"
                        start_recording()
                    elif one_key is not None and k in one_key.key_family:
                        _mode = "one_key"
                        start_recording()
                elif _mode == "one_key":
                    if (
                        one_key is not None
                        and k not in one_key.key_family
                        and not _chord_involved(k)
                    ):
                        _mode = "lockout"
                        cancel_recording()

        def on_release(key: Any) -> None:
            nonlocal _mode
            k = kb_listener.canonical(key) if kb_listener else key
            with _mode_lock:
                held.discard(k)
                if _mode == "chord" and _chord_involved(k):
                    _mode = "idle"
                    stop_recording()
                elif (
                    _mode == "one_key"
                    and one_key is not None
                    and k in one_key.key_family
                ):
                    _mode = "idle"
                    stop_recording()
                elif (
                    _mode == "lockout"
                    and one_key is not None
                    and k in one_key.key_family
                ):
                    _mode = "idle"

        kb_listener = keyboard.Listener(on_press=on_press, on_release=on_release)
        listeners.append(kb_listener)

    if isinstance(trig, MouseTrigger) and one_key is not None:
        ok_listener: keyboard.Listener | None = None

        def on_ok_press(key: Any) -> None:
            nonlocal _mode
            k = ok_listener.canonical(key) if ok_listener else key
            with _mode_lock:
                if _mode == "idle" and k in one_key.key_family:
                    _mode = "one_key"
                    start_recording()
                elif _mode == "one_key" and k not in one_key.key_family:
                    _mode = "lockout"
                    cancel_recording()

        def on_ok_release(key: Any) -> None:
            nonlocal _mode
            k = ok_listener.canonical(key) if ok_listener else key
            with _mode_lock:
                if _mode == "one_key" and k in one_key.key_family:
                    _mode = "idle"
                    stop_recording()
                elif _mode == "lockout" and k in one_key.key_family:
                    _mode = "idle"

        ok_listener = keyboard.Listener(on_press=on_ok_press, on_release=on_ok_release)
        listeners.append(ok_listener)

    return listeners


def _log_file_path() -> Path:
    """Default log path for Show log / future --log-file features.

    Honours ``GHOSTSCRIBE_LOG_FILE``; otherwise falls back to
    ``$XDG_STATE_HOME/ghostscribe/ghostscribe.log`` (or
    ``~/.local/state/ghostscribe/ghostscribe.log``)."""
    env = os.environ.get("GHOSTSCRIBE_LOG_FILE")
    if env:
        return Path(env)
    state = os.environ.get("XDG_STATE_HOME") or str(Path.home() / ".local" / "state")
    return Path(state) / "ghostscribe" / "ghostscribe.log"


def _open_path_in_editor(path: Path) -> None:
    """Resolve an editor in ``$VISUAL`` -> ``$EDITOR`` -> ``xdg-open`` order.

    If ``$VISUAL``/``$EDITOR`` is set and there's a terminal emulator on
    ``$PATH`` we launch the editor in a new terminal window so the user
    doesn't need a pre-existing shell; otherwise we fall through to
    ``xdg-open`` and let the desktop's file association pick a GUI tool.
    """
    for var in ("VISUAL", "EDITOR"):
        ed = os.environ.get(var, "").strip()
        if not ed:
            continue
        term = None
        for cand in ("x-terminal-emulator", "gnome-terminal", "konsole", "xterm"):
            if shutil.which(cand):
                term = cand
                break
        if term is not None:
            try:
                subprocess.Popen([term, "-e", f'{ed} "{path}"'])
                return
            except OSError:
                continue
    # Final fallback: let the desktop handle it.
    if shutil.which("xdg-open"):
        subprocess.Popen(["xdg-open", str(path)])
    else:
        _eprint(f"[tray] no editor available; file is at {path}")


def _reveal_in_file_manager(path: Path) -> None:
    parent = path.parent
    if shutil.which("xdg-open"):
        subprocess.Popen(["xdg-open", str(parent)])
    else:
        _eprint(f"[tray] no file manager available; folder is {parent}")


def _seed_config_if_missing(path: Path) -> None:
    if path.exists():
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(_config.DEFAULT_CONFIG_TOML, encoding="utf-8")
    try:
        path.chmod(0o600)
    except OSError:
        pass


def run_tray(initial: ClientConfig) -> int:
    """Tray-driven run loop. Mirrors :func:`run` but routes transitions
    through a pystray icon and applies safe config changes live.

    Cold keys (``trigger``, ``one_key_trigger``, ``input_device``,
    ``audio_format``) are captured here at startup; edits that touch
    them surface a "Restart required" tooltip, and the Restart menu
    item respawns the process via :func:`os.execv`.
    """
    log_fh = _setup_tray_log()

    try:
        trig = parse_trigger(initial.trigger)
    except ValueError as exc:
        _eprint(f"[fatal] {exc}")
        return 2
    try:
        one_key = parse_one_key_trigger(initial.one_key_trigger)
    except ValueError as exc:
        _eprint(f"[fatal] {exc}")
        return 2

    device = _resolve_input_device(initial.input_device)
    _print_banner(initial, mode="tray")

    # Thread-safe mutable config holder. Python lists are used as a
    # single-cell "box" here; the lock guards *both* read and replace so
    # a reader never observes a torn update across unrelated threads.
    cfg_lock = threading.Lock()
    cfg_box: list[ClientConfig] = [initial]

    def get_cfg() -> ClientConfig:
        with cfg_lock:
            return cfg_box[0]

    def set_cfg(new: ClientConfig) -> None:
        with cfg_lock:
            cfg_box[0] = new

    pending_restart: set[str] = set()
    pending_lock = threading.Lock()

    recorder = Recorder(device)
    try:
        recorder.start_stream()
    except sd.PortAudioError as exc:
        _eprint(f"[fatal] could not open audio input: {exc}")
        return 1

    jobs: queue.Queue[np.ndarray | None] = queue.Queue()
    stop_event = threading.Event()
    recording = threading.Event()

    # Forward-declared reference so callbacks can touch the tray after
    # it's constructed below.
    tray_box: list[_tray.Tray | None] = [None]

    def on_state(state: TrayState, suffix: str = "") -> None:
        t = tray_box[0]
        if t is not None:
            t.set_state(state, suffix)

    def set_suffix(suffix: str) -> None:
        t = tray_box[0]
        if t is not None:
            t.set_tooltip_suffix(suffix)

    def mark_restart(cold_keys: tuple[str, ...]) -> None:
        with pending_lock:
            pending_restart.update(cold_keys)
            snapshot = tuple(sorted(pending_restart))
        on_state(TrayState.ERROR, f"restart required: {', '.join(snapshot)}")
        _eprint(f"[config] restart required: {', '.join(snapshot)}")

    http = httpx.Client()

    def worker() -> None:
        while True:
            item = jobs.get()
            if item is None:
                return
            on_state(TrayState.UPLOADING)
            try:
                result = submit(get_cfg(), http, item)
            except Exception as exc:
                _eprint(f"[send] unexpected error: {exc}")
                on_state(TrayState.ERROR, str(exc)[:80])
                continue
            if result is None:
                on_state(TrayState.ERROR, "upload failed")
            elif result:
                on_state(TrayState.IDLE, f"last: {len(result)} chars")
            else:
                on_state(TrayState.IDLE, "empty transcript")
            if recording.is_set():
                on_state(TrayState.RECORDING)

    worker_thread = threading.Thread(target=worker, name="gs-submit", daemon=True)
    worker_thread.start()

    def _on_auto_chunk() -> None:
        audio = recorder.checkpoint()
        if audio is not None and len(audio) > 0:
            _eprint(f"[rec] auto-chunk, {len(audio) * 2 / 1024:.0f} kB")
            jobs.put(audio)

    _chunk_timer = _ChunkTimer(_on_auto_chunk)

    def start_recording() -> None:
        if recording.is_set():
            return
        recording.set()
        recorder.begin()
        on_state(TrayState.RECORDING)
        _eprint("[rec] ...")
        _chunk_timer.start()

    def stop_recording() -> None:
        _chunk_timer.stop()
        if not recording.is_set():
            return
        recording.clear()
        audio = recorder.end()
        n = 0 if audio is None else len(audio)
        _eprint(f"[rec] stopped, {n * 2 / 1024:.0f} kB raw")
        jobs.put(audio)
        # worker() will transition to UPLOADING once it dequeues.

    def cancel_recording() -> None:
        _chunk_timer.stop()
        if not recording.is_set():
            return
        recording.clear()
        recorder.cancel()
        on_state(TrayState.IDLE)
        _eprint("[rec] cancelled")

    listeners = _build_listeners(
        trig, one_key, start_recording, stop_recording, cancel_recording
    )
    for ls in listeners:
        ls.start()

    # Config watcher.
    watcher_thread: threading.Thread | None = None

    def on_watcher_event(event: "_watcher.WatcherEvent") -> None:
        if isinstance(event, _watcher.ReloadedEvent):
            set_cfg(event.new_config)
            if event.diff.hot_changed:
                msg = f"reloaded: {', '.join(event.diff.hot_changed)}"
                _eprint(f"[config] {msg}")
                set_suffix(msg)
            if event.diff.cold_changed:
                mark_restart(event.diff.cold_changed)
        elif isinstance(event, _watcher.ParseErrorEvent):
            _eprint(f"[config] parse error: {event.message}")
            on_state(TrayState.ERROR, "config parse error — see log")
            t = tray_box[0]
            if t is not None:
                t.notify("GhostScribe — config parse error", event.message)
        elif isinstance(event, _watcher.MissingEvent):
            _eprint("[config] source file disappeared")
            set_suffix("config file missing")

    if initial.source_path is not None:
        watcher_thread = _watcher.spawn(
            initial.source_path, get_cfg, on_watcher_event, stop_event
        )

    def on_action(action: MenuAction) -> None:
        cfg_now = get_cfg()
        if action == MenuAction.QUIT:
            stop_event.set()
            t = tray_box[0]
            if t is not None:
                _tray.stop(t)
        elif action == MenuAction.EDIT_CONFIG:
            path = cfg_now.source_path or _config._candidate_paths(None)[0]
            try:
                _seed_config_if_missing(path)
            except OSError as exc:
                _eprint(f"[tray] could not seed {path}: {exc}")
                return
            _open_path_in_editor(path)
        elif action == MenuAction.REVEAL_CONFIG:
            if cfg_now.source_path is not None:
                _reveal_in_file_manager(cfg_now.source_path)
        elif action == MenuAction.RELOAD_CONFIG:
            if cfg_now.source_path is None:
                set_suffix("no config file to reload")
                return
            try:
                new_cfg = _config.load_from(cfg_now.source_path)
            except Exception as exc:
                _eprint(f"[config] parse error: {exc}")
                on_state(TrayState.ERROR, "config parse error — see log")
                t = tray_box[0]
                if t is not None:
                    t.notify(
                        "GhostScribe — config parse error",
                        f"{type(exc).__name__}: {exc}",
                    )
                return
            d = _config.diff(cfg_now, new_cfg)
            set_cfg(new_cfg)
            if d.is_empty():
                set_suffix("reload: no changes")
            else:
                if d.hot_changed:
                    set_suffix(f"reloaded: {', '.join(d.hot_changed)}")
                if d.cold_changed:
                    mark_restart(d.cold_changed)
        elif action == MenuAction.TOGGLE_LOG:
            if isinstance(sys.stderr, _TeeStream):
                sys.stderr.paused = not sys.stderr.paused
                state = "on" if not sys.stderr.paused else "off"
                _eprint(f"[log] logging {state}")
                set_suffix(f"logging {state}")
        elif action == MenuAction.SHOW_LOG:
            lp = _log_file_path()
            if lp.exists():
                _open_path_in_editor(lp)
            else:
                set_suffix(f"no log file at {lp}")
        elif action == MenuAction.RESTART:
            stop_event.set()
            # os.execv replaces the process image; argv is passed through
            # unchanged so --no-tray is preserved if the user set it.
            try:
                os.execv(sys.executable, [sys.executable, "-m", "ghostscribe_client", *sys.argv[1:]])
            except OSError as exc:
                _eprint(f"[tray] restart failed: {exc}")
        elif action == MenuAction.ABOUT:
            c = get_cfg()
            t = tray_box[0]
            if t is not None:
                t.notify(
                    "GhostScribe",
                    f"server: {c.url}\nconfig: {c.source_path or '(defaults)'}",
                )

    def get_logging() -> bool:
        return isinstance(sys.stderr, _TeeStream) and not sys.stderr.paused

    try:
        tray_obj = _tray.build_tray(on_action, initial.source_path, get_logging)
    except RuntimeError as exc:
        _eprint(f"[fatal] {exc}")
        for ls in listeners:
            ls.stop()
        recorder.stop_stream()
        http.close()
        return 1
    tray_box[0] = tray_obj

    def _stop(_sig: int = 0, _frame: Any = None) -> None:
        stop_event.set()
        _tray.stop(tray_obj)

    signal.signal(signal.SIGINT, _stop)
    try:
        signal.signal(signal.SIGTERM, _stop)
    except (AttributeError, ValueError):
        pass

    try:
        _tray.run_blocking(tray_obj)  # Blocks on the main thread.
    finally:
        _eprint("Shutting down...")
        stop_event.set()
        for ls in listeners:
            ls.stop()
        jobs.put(None)
        worker_thread.join(timeout=5.0)
        if watcher_thread is not None:
            watcher_thread.join(timeout=2.0)
        recorder.stop_stream()
        http.close()
        if log_fh is not None:
            sys.stderr = sys.stderr._orig  # type: ignore[union-attr]
            log_fh.close()

    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="ghostscribe-client", description=__doc__)
    p.add_argument("--config", type=Path, help="Path to a TOML config file.")
    p.add_argument("--server-url", help="Override server_url.")
    p.add_argument("--endpoint", help="Override endpoint (e.g. /v1/en).")
    p.add_argument(
        "--trigger",
        help='Override trigger, e.g. "mouse:x2" or "key:ctrl+g".',
    )
    p.add_argument("--auth-token", help="Override auth token.")
    p.add_argument("--input-device", help="Override audio input device (name or index).")
    p.add_argument(
        "--audio-format",
        choices=("flac", "wav"),
        help="Override audio_format.",
    )
    paste = p.add_mutually_exclusive_group()
    paste.add_argument(
        "--paste",
        dest="auto_paste",
        action="store_const",
        const=True,
        help="Copy to clipboard and simulate Ctrl+V (default).",
    )
    paste.add_argument(
        "--no-paste",
        dest="auto_paste",
        action="store_const",
        const=False,
        help="Do not touch the clipboard or inject Ctrl+V.",
    )
    p.add_argument(
        "--paste-delay-ms",
        type=int,
        help="Milliseconds to wait between clipboard write and Ctrl+V.",
    )
    p.add_argument(
        "--no-tray",
        dest="no_tray",
        action="store_true",
        help=(
            "Run in command-line mode (no tray icon). "
            "Logs to stderr only. Default is tray mode."
        ),
    )
    return p.parse_args(argv)


def apply_overrides(cfg: ClientConfig, args: argparse.Namespace) -> ClientConfig:
    changes: dict[str, Any] = {}
    for cli_name, cfg_name in [
        ("server_url", "server_url"),
        ("endpoint", "endpoint"),
        ("trigger", "trigger"),
        ("auth_token", "auth_token"),
        ("input_device", "input_device"),
        ("audio_format", "audio_format"),
        ("auto_paste", "auto_paste"),
        ("paste_delay_ms", "paste_delay_ms"),
    ]:
        value = getattr(args, cli_name)
        if value is not None:
            changes[cfg_name] = value
    if not changes:
        return cfg
    return ClientConfig(
        server_url=changes.get("server_url", cfg.server_url),
        endpoint=changes.get("endpoint", cfg.endpoint),
        trigger=changes.get("trigger", cfg.trigger),
        one_key_trigger=cfg.one_key_trigger,
        auth_token=changes.get("auth_token", cfg.auth_token),
        input_device=changes.get("input_device", cfg.input_device),
        audio_format=changes.get("audio_format", cfg.audio_format),
        auto_paste=changes.get("auto_paste", cfg.auto_paste),
        paste_delay_ms=changes.get("paste_delay_ms", cfg.paste_delay_ms),
        source_path=cfg.source_path,
    )


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    cfg = load_config(args.config)
    cfg = apply_overrides(cfg, args)
    if getattr(args, "no_tray", False):
        return run(cfg)
    return run_tray(cfg)


if __name__ == "__main__":
    raise SystemExit(main())
