"""GhostScribe minimal PTT client.

Hold the configured key; while it's held, 16 kHz mono 16-bit audio is
captured. On key release the buffer is assembled into a WAV in memory,
POSTed to the configured endpoint, and the returned transcript is printed
to stdout. Timing / status info goes to stderr.

Usage:
    python -m ghostscribe_client
    python -m ghostscribe_client --config ~/my_gs.toml
    python -m ghostscribe_client --endpoint /v1/sk

Press Ctrl+C in the terminal to exit.
"""

from __future__ import annotations

import argparse
import io
import queue
import signal
import sys
import threading
import time
from pathlib import Path
from typing import Any

import numpy as np
import sounddevice as sd
import soundfile as sf
import httpx
from pynput import keyboard

from .config import ClientConfig, load_config

SAMPLE_RATE = 16_000
CHANNELS = 1
DTYPE = "int16"


# --------------------------------------------------------------------------- #
# Helpers                                                                     #
# --------------------------------------------------------------------------- #


def _eprint(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def _resolve_ptt_key(name: str) -> Any:
    """Translate a string like ctrl_r / f12 / backtick to a pynput key."""
    name = name.strip()
    if not name:
        raise ValueError("ptt_key is empty")
    # Try special keys (Key.ctrl_r, Key.f12, etc.)
    special = getattr(keyboard.Key, name, None)
    if isinstance(special, keyboard.Key):
        return special
    # Fall back to a character key.
    if len(name) == 1:
        return keyboard.KeyCode.from_char(name)
    raise ValueError(
        f"unknown ptt_key {name!r}. Use names from pynput.keyboard.Key "
        "(e.g. ctrl_r, alt_r, f12) or a single character."
    )


def _resolve_input_device(raw: str) -> int | str | None:
    raw = raw.strip()
    if not raw:
        return None
    if raw.lstrip("-").isdigit():
        return int(raw)
    return raw


def _keys_equal(a: Any, b: Any) -> bool:
    """Compare two pynput keys, tolerating KeyCode vs Key variants."""
    if a == b:
        return True
    # Some platforms emit KeyCode with .vk but no .char; compare string form as a fallback.
    try:
        if isinstance(a, keyboard.KeyCode) and isinstance(b, keyboard.KeyCode):
            return a.char == b.char and a.vk == b.vk
    except AttributeError:
        pass
    return False


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

    def end(self) -> bytes:
        """Stop capturing and return the collected audio as a WAV byte string."""
        self._active.clear()
        with self._lock:
            chunks = self._chunks
            self._chunks = []
        if not chunks:
            return b""
        audio = np.concatenate(chunks, axis=0)
        buf = io.BytesIO()
        sf.write(buf, audio, SAMPLE_RATE, subtype="PCM_16", format="WAV")
        return buf.getvalue()


# --------------------------------------------------------------------------- #
# HTTP submission                                                             #
# --------------------------------------------------------------------------- #


def submit(cfg: ClientConfig, client: httpx.Client, wav_bytes: bytes) -> None:
    if not wav_bytes:
        _eprint("[send] skipped: no audio captured")
        return

    headers: dict[str, str] = {}
    if cfg.has_auth:
        headers["X-Auth-Token"] = cfg.auth_token

    files = {"audio": ("recording.wav", wav_bytes, "audio/wav")}
    size_kb = len(wav_bytes) / 1024
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
    if text:
        print(text, flush=True)
    else:
        _eprint("[recv] empty transcript")


# --------------------------------------------------------------------------- #
# Main loop                                                                   #
# --------------------------------------------------------------------------- #


def run(cfg: ClientConfig) -> int:
    ptt = _resolve_ptt_key(cfg.ptt_key)
    device = _resolve_input_device(cfg.input_device)

    _eprint(f"GhostScribe client -> {cfg.url}")
    if cfg.source_path is not None:
        _eprint(f"config:   {cfg.source_path}")
    else:
        _eprint("config:   (defaults, no config file found)")
    _eprint(f"ptt key:  {cfg.ptt_key}")
    _eprint(f"device:   {device if device is not None else '(system default)'}")
    _eprint(f"auth:     {'on' if cfg.has_auth else 'off'}")
    _eprint("Hold the PTT key and speak. Release to transcribe. Ctrl+C to quit.")

    recorder = Recorder(device)
    try:
        recorder.start_stream()
    except sd.PortAudioError as exc:
        _eprint(f"[fatal] could not open audio input: {exc}")
        return 1

    jobs: queue.Queue[bytes | None] = queue.Queue()
    pressed = threading.Event()
    stop = threading.Event()

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

        def on_press(key: Any) -> None:
            if _keys_equal(key, ptt) and not pressed.is_set():
                pressed.set()
                recorder.begin()
                _eprint("[rec] ...")

        def on_release(key: Any) -> None:
            if _keys_equal(key, ptt) and pressed.is_set():
                pressed.clear()
                wav_bytes = recorder.end()
                _eprint(f"[rec] stopped, {len(wav_bytes) / 1024:.0f} kB")
                jobs.put(wav_bytes)

        listener = keyboard.Listener(on_press=on_press, on_release=on_release)
        listener.start()

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
            listener.stop()
            jobs.put(None)
            worker_thread.join(timeout=5.0)
            recorder.stop_stream()

    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="ghostscribe-client", description=__doc__)
    p.add_argument("--config", type=Path, help="Path to a TOML config file.")
    p.add_argument("--server-url", help="Override server_url.")
    p.add_argument("--endpoint", help="Override endpoint (e.g. /v1/en).")
    p.add_argument("--ptt-key", help="Override PTT key name.")
    p.add_argument("--auth-token", help="Override auth token.")
    p.add_argument("--input-device", help="Override audio input device (name or index).")
    return p.parse_args(argv)


def apply_overrides(cfg: ClientConfig, args: argparse.Namespace) -> ClientConfig:
    changes: dict[str, Any] = {}
    for cli_name, cfg_name in [
        ("server_url", "server_url"),
        ("endpoint", "endpoint"),
        ("ptt_key", "ptt_key"),
        ("auth_token", "auth_token"),
        ("input_device", "input_device"),
    ]:
        value = getattr(args, cli_name)
        if value is not None:
            changes[cfg_name] = value
    if not changes:
        return cfg
    return ClientConfig(
        server_url=changes.get("server_url", cfg.server_url),
        endpoint=changes.get("endpoint", cfg.endpoint),
        ptt_key=changes.get("ptt_key", cfg.ptt_key),
        auth_token=changes.get("auth_token", cfg.auth_token),
        input_device=changes.get("input_device", cfg.input_device),
        source_path=cfg.source_path,
    )


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    cfg = load_config(args.config)
    cfg = apply_overrides(cfg, args)
    return run(cfg)


if __name__ == "__main__":
    raise SystemExit(main())
