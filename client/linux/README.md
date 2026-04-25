# GhostScribe — Linux Python client

A push-to-talk client for Linux, written in Python 3. Uses `pynput` for
global input monitoring, `sounddevice` for microphone capture, and `xclip`
for clipboard integration.

- Hold the configured trigger (default `mouse:x2` — thumb-forward mouse button) → microphone starts recording.
- Release the trigger → buffer is encoded as **FLAC** (or WAV) and POSTed to the server.
- Optional `one_key_trigger` lets a single modifier or F-key act as PTT; pressing a foreign key mid-take cancels the recording without sending.
- If `auto_paste = true` (the default), the transcript is written to the X11 CLIPBOARD via `xclip`, the target window is detected (terminal vs. GUI), and Ctrl+V or Ctrl+Shift+V is synthesised to paste it.

## 1. Install

### System packages

```sh
sudo apt install xclip xdotool x11-utils
```

- **`xclip`** — required for clipboard read/write.
- **`xdotool`** and **`x11-utils`** (`xprop`) — optional, but needed for terminal-aware paste. Without them, terminal windows receive plain Ctrl+V (which most terminals ignore as a paste shortcut).

### Python package

Python 3.10 or later is required.

```sh
cd client/linux
pip install -e .           # installs ghostscribe-client + all deps
```

Or install deps manually:

```sh
pip install -r requirements.txt
```

For the system-tray icon, also install `PyGObject`:

```sh
sudo apt install python3-gi python3-gi-cairo gir1.2-gtk-3.0
pip install -e ".[tray]"
```

## 2. Configure

Drop a `config.toml` at `~/.config/ghostscribe/config.toml` (or pass
`--config PATH`). Start from `config.example.toml` in this folder:

```toml
server_url      = "http://SERVER_HOST:5005"
endpoint        = "/v1/auto"       # /v1/auto | /v1/en
auth_token      = ""               # same value as the Rust clients
input_device    = ""               # empty = system default mic
audio_format    = "flac"           # "flac" (smaller) or "wav"
trigger         = "mouse:x2"       # mouse:<button>  OR  key:[mod+…+]<key>
                                   # mouse buttons: left, right, middle, x1 (back), x2 (forward)
one_key_trigger = ""               # empty, or key:ctrl|alt|f1..f24
auto_paste      = true             # false => stderr only
paste_delay_ms  = 50               # ms to wait before Ctrl+V; restore uses max(this, 150 ms)
```

All fields are optional; omitted keys fall back to the defaults above.

| Key | Default | Notes |
| --- | --- | --- |
| `server_url` | `http://localhost:5005` | No trailing slash. |
| `endpoint` | `/v1/auto` | `/v1/en` forces English transcription. |
| `auth_token` | empty | Sent as `X-Auth-Token` when non-empty. **Do not commit your real token.** |
| `input_device` | empty | Substring of the sounddevice device name, or an integer index. Empty = system default. Run `python -c "import sounddevice; print(sounddevice.query_devices())"` to list devices. |
| `audio_format` | `flac` | `flac` halves payload vs raw WAV. Use `wav` only for debugging. |
| `trigger` | `mouse:x2` | Mouse: `mouse:<button>` — `left`, `right`, `middle`, `x1` (back), `x2` (forward), `back`, `forward`. Keyboard chord: `key:[mod+…+]<key>`. Modifiers: `ctrl`, `shift`, `alt`, `super`. Keys: pynput `Key` names (`f12`, `ctrl_r`, `pause`, …) or a single character. Examples: `key:ctrl+g`, `key:ctrl+shift+space`. |
| `one_key_trigger` | empty | Optional single-key PTT. Allowed: `key:ctrl`, `key:alt`, `key:f1`–`key:f24`. Foreign key mid-record cancels. |
| `auto_paste` | `true` | Save clipboard → set transcript → detect terminal → Ctrl+V or Ctrl+Shift+V → restore clipboard. |
| `paste_delay_ms` | `50` | Sleep before injecting Ctrl+V. Clipboard restore uses `max(paste_delay_ms, 150 ms)` to avoid racing ahead of the target window's clipboard read. |
| `request_timeout_s` | `30` | HTTP POST timeout in seconds. |
| `smart_space` | `true` | Prepends a space when pasting within `continuation_window_s` of the last paste (continuation dictation). |
| `continuation_window_s` | `30` | Seconds after the last paste that counts as a continuation. |
| `max_record_s` | `300` | Auto-stop recording after this many seconds. `0` = off. Active in all modes. |

**One-key trigger semantics.** When `one_key_trigger` is set, the client
accepts two ways to record: the chord in `trigger`, or the single key in
`one_key_trigger`. Press alone → record; release alone → send. Press any
other key while recording via one-key → cancel the take (no upload) and
lock out until the one-key is released. Keys that are part of `trigger`
are treated as neutral. Shift, letters, and digits are rejected as one-key
triggers because they would hijack normal typing.

Config search order:

1. `--config PATH` CLI argument
2. `$XDG_CONFIG_HOME/ghostscribe/config.toml`
3. `~/.config/ghostscribe/config.toml`
4. `./config.toml` (current working directory)

**Never commit a filled-in `config.toml`** containing a real `auth_token`.

## 3. Run

### Tray mode (default, recommended)

```sh
python -m ghostscribe_client
```

Tray mode runs with a colour-coded system-tray icon and logs to
`~/.local/state/ghostscribe/ghostscribe.log`. Left-click the icon for
the context menu:

- **Edit config…** — opens `config.toml` in `$VISUAL`/`$EDITOR` or via `xdg-open`. Seeds a commented template if the file does not exist yet.
- **Reveal config** — opens the config directory in the file manager.
- **Reload now** — force-revalidates the config on demand.
- **Show log** — opens the log file in your editor.
- **Logging on/off** — toggle writing to the log file (stderr always gets every line).
- **Restart client** — respawns via `os.execv` with the same arguments. Use after changing a cold key.
- **About GhostScribe** — server URL and config path.
- **Quit** — exit.

**Icon colour reflects state:** idle (grey), recording (red), uploading
(blue), error (amber).

**Live config reload.** A watcher thread polls the config file every second:

- **Hot keys** (`server_url`, `endpoint`, `auth_token`, `auto_paste`,
  `paste_delay_ms`, `request_timeout_s`, `smart_space`,
  `continuation_window_s`, `max_record_s`) take effect immediately on
  the next upload/paste.
- **Cold keys** (`trigger`, `one_key_trigger`, `input_device`,
  `audio_format`) require a restart (icon turns amber, tooltip shows
  `restart required: trigger`).
- **Parse errors** show a desktop notification and leave the running
  config untouched.

### CLI / headless mode

```sh
python -m ghostscribe_client --no-tray
```

No tray icon; all output goes to stderr. Useful in SSH sessions or when
`pystray` / GTK is unavailable.

### Foreground banner (both modes)

```
GhostScribe client -> http://SERVER_HOST:5005/v1/auto
config:   /home/you/.config/ghostscribe/config.toml
trigger:  mouse:x2
one_key:  off
device:   (system default)
format:   flac
paste:    on (delay 50 ms)
auth:     off
Hold the trigger and speak. Release to transcribe. Ctrl+C to quit.
```

Hold the trigger and speak, then release:

```
[rec] ...
[rec] stopped, 98 kB raw
[recv] 51 kB in 380 ms (lang=en p=0.98)
[paste] window='firefox' -> ctrl+v
[paste] clipboard restored
[paste] pasted via ctrl+v into focused window:
Hello, this is a test transcription.
```

## 4. Prerequisites

- **Linux with X11** (or XWayland). Wayland native global hotkeys are not
  supported; the pynput XRecord backend works under XWayland for most
  apps.
- **Python ≥ 3.10**.
- **`xclip`** for clipboard: `sudo apt install xclip`
- **`xdotool` + `xprop`** for terminal-aware paste (Ctrl+Shift+V):
  `sudo apt install xdotool x11-utils`
- **`libappindicator3`** or **`libayatana-appindicator3`** for the tray
  icon (tray mode only).
- Default microphone configured in PulseAudio / PipeWire.
- Network reachability to the server on port 5005.

No root access is needed.

## 5. What this client does and does not do

**Implemented:**

- Configurable trigger via `pynput`: keyboard chord, one-key PTT (modifier
  or F-key), or mouse button.
- Mouse PTT: `mouse:left/right/middle/x1/x2/back/forward` and raw
  `mouse:button8` / `mouse:button9`.
- Multi-modifier chords: `key:ctrl+shift+g`, `key:super+space`, etc.
- Microphone capture via `sounddevice` (PulseAudio/PipeWire), 16 kHz mono.
- FLAC or WAV encoding via `soundfile`.
- Multipart HTTP(S) POST via `httpx`, optional `X-Auth-Token`.
- Terminal-aware paste: detects focused window class via `xdotool` +
  `xprop` and sends Ctrl+Shift+V to terminal emulators that ignore plain
  Ctrl+V.
- Save-Paste-Restore clipboard: `xclip` read/write with a `max(paste_delay_ms, 150 ms)` restore delay.
- Auto-chunk uploads every 2 minutes while recording (all modes).
- `max_record_s` hard cap on recording length (all modes).
- Tray mode with state icon, live config reload, log toggle, and restart.
- `--no-tray` CLI mode; logs to stderr only.
- `do-it-now` → injects Enter instead of paste.

**Not implemented:**

- No installer or `.desktop` autostart entry.
- No client-side VAD (server-side VAD handles silence).
- No streaming / live partial transcripts.
- Wayland native global hotkeys (XWayland fallback works for most apps).
- No clipboard-change verification before restore (if you copy something
  between the paste and the restore, the restore will clobber it).

## 6. Files in this folder

| File | Purpose |
| --- | --- |
| `ghostscribe_client/__main__.py` | Entry point; trigger parsing, audio capture, clipboard paste, tray/headless run loops. |
| `ghostscribe_client/config.py` | TOML config loader with hot/cold key classification and diff. |
| `ghostscribe_client/tray.py` | `pystray`-based tray icon, state colours, menu actions. |
| `ghostscribe_client/watcher.py` | 1 s mtime poll of the active `config.toml`. |
| `ghostscribe_client/__init__.py` | Package marker. |
| `tests/` | 100-test pytest suite covering clipboard, trigger parsing, config, submit pipeline, tray, and watcher. |
| `pyproject.toml` | Package metadata and dev dependencies. |
| `requirements.txt` | Pinned runtime dependencies for `pip install -r`. |
| `config.example.toml` | Commented template config (safe to commit). |
