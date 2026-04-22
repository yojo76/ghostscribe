"""GhostScribe minimal PTT client.

Hold the configured trigger (a mouse button or keyboard combo); while it's
held, 16 kHz mono 16-bit audio is captured. On release the buffer is
encoded (FLAC by default), POSTed to the configured endpoint, and the
returned transcript is pushed to the X11 CLIPBOARD via ``xclip``. Timing
and status info goes to stderr.

Usage:
    python -m ghostscribe_client
    python -m ghostscribe_client --config ~/my_gs.toml
    python -m ghostscribe_client --trigger key:ctrl+g

Press Ctrl+C in the terminal to exit.
"""

from __future__ import annotations

import argparse
import io
import queue
import shutil
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np
import sounddevice as sd
import soundfile as sf
import httpx
from pynput import keyboard, mouse

from .config import ClientConfig, load_config

SAMPLE_RATE = 16_000
CHANNELS = 1
DTYPE = "int16"


# --------------------------------------------------------------------------- #
# Helpers                                                                     #
# --------------------------------------------------------------------------- #


def _eprint(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


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


def detect_terminal_focus() -> bool:
    """Return True if the X11 foreground window looks like a terminal emulator.

    Uses ``xdotool getactivewindow getwindowclassname``; if xdotool is missing
    or the call fails we silently return False, which preserves the previous
    Ctrl+V-only behaviour.
    """
    xdotool = shutil.which("xdotool")
    if xdotool is None:
        return False
    try:
        result = subprocess.run(
            [xdotool, "getactivewindow", "getwindowclassname"],
            capture_output=True,
            timeout=2,
        )
    except subprocess.SubprocessError:
        return False
    if result.returncode != 0:
        return False
    cls = result.stdout.decode("utf-8", errors="replace").strip()
    return cls.lower() in _TERMINAL_CLASSES


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
) -> None:
    if audio is None or len(audio) == 0:
        _eprint("[send] skipped: no audio captured")
        return

    try:
        payload, filename, mime = encode_audio(audio, cfg.audio_format)
    except Exception as exc:
        _eprint(f"[send] encoding failed: {exc}")
        return

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
        return
    dt_ms = (time.perf_counter() - t0) * 1000

    if resp.status_code >= 400:
        _eprint(f"[send] HTTP {resp.status_code}: {resp.text.strip()}")
        return
    try:
        data = resp.json()
    except ValueError:
        _eprint(f"[send] non-JSON response: {resp.text[:200]}")
        return

    text = (data.get("text") or "").strip()
    lang = data.get("language", "?")
    prob = data.get("language_probability", 0)
    _eprint(f"[recv] {size_kb:.0f} kB in {dt_ms:.0f} ms (lang={lang} p={prob})")
    if not text:
        _eprint("[recv] empty transcript")
        return

    pasted = False
    combo_used = "ctrl+v"
    if cfg.auto_paste:
        saved = read_clipboard()
        # Trailing space so back-to-back takes don't concatenate in the
        # target field. Only applied to the pasted copy — not to _eprint().
        if copy_to_clipboard(text + " "):
            use_shift = detect_terminal_focus()
            combo_used = "ctrl+shift+v" if use_shift else "ctrl+v"
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
    if cfg.auto_paste and shutil.which("xdotool") is None:
        _eprint(
            "[warn] xdotool not found; terminal-focused windows will receive "
            "Ctrl+V instead of Ctrl+Shift+V. Install with: sudo apt install xdotool"
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

        def start_recording() -> None:
            if recording.is_set():
                return
            recording.set()
            recorder.begin()
            _eprint("[rec] ...")

        def stop_recording() -> None:
            if not recording.is_set():
                return
            recording.clear()
            audio = recorder.end()
            n = 0 if audio is None else len(audio)
            _eprint(f"[rec] stopped, {n * 2 / 1024:.0f} kB raw")
            jobs.put(audio)

        def _cancel_recording() -> None:
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
    return run(cfg)


if __name__ == "__main__":
    raise SystemExit(main())
