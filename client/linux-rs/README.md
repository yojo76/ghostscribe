# GhostScribe - Linux client (Rust)

A push-to-talk client for Linux, written in Rust. Ships as a single
self-contained binary with no Python dependency.

- Hold the configured trigger (default `Ctrl+G`) → microphone starts recording.
- Release the trigger → buffer is downmixed to mono, resampled to 16 kHz,
  encoded as **FLAC** (or WAV if configured) and POSTed to the server.
- Optional `one_key_trigger` (e.g. `key:ctrl`) lets a single modifier or
  F-key act as PTT; pressing a foreign key mid-take cancels without sending.
- Mouse PTT is supported: `trigger = "mouse:x2"` binds the forward side button.
- Transcript text goes to stdout; status/timing goes to stderr.
- If `auto_paste = true` (the default), the transcript is written to the
  clipboard and `Ctrl+V` is synthesised into the focused window.

## 1. Build

```sh
# Install system dependencies (Ubuntu / Debian)
sudo apt install \
    libappindicator3-dev libgtk-3-dev \
    libxtst-dev libx11-dev

cd client/linux-rs
cargo build --release
# Output: target/release/ghostscribe-client
```

See `BUILD.md` for full details and cross-compile instructions.

## 2. Configure

Drop a `config.toml` next to the binary, or at
`$XDG_CONFIG_HOME/ghostscribe/config.toml` (defaults to
`~/.config/ghostscribe/config.toml`). Start from the commented template
that `--tray` → **Edit config…** seeds automatically, or write one by hand:

```toml
server_url      = "http://SERVER_HOST:5005"
endpoint        = "/v1/auto"       # /v1/auto | /v1/en
auth_token      = ""               # same value as the Python / Windows client
input_device    = ""               # empty = ALSA/PulseAudio default mic
audio_format    = "flac"           # "flac" (smaller) or "wav"
trigger         = "key:ctrl+g"     # key:[modifier+]<keyname>  OR  mouse:<button>
                                   # mouse buttons: left, right, middle, x1 (back), x2 (forward)
one_key_trigger = ""               # empty, or key:ctrl|alt|f1..f12
auto_paste      = true             # false => stdout only
paste_delay_ms  = 50               # ms before Ctrl+V and before clipboard restore
```

All fields are optional; omitted keys fall back to the defaults above.

| Key               | Default              | Notes                                                                                      |
| ----------------- | -------------------- | ------------------------------------------------------------------------------------------ |
| `server_url`      | `http://localhost:5005` | No trailing slash.                                                                      |
| `endpoint`        | `/v1/auto`           | `/v1/en` forces English transcription.                                                    |
| `auth_token`      | empty                | Sent as `X-Auth-Token` when non-empty.                                                    |
| `input_device`    | empty                | Case-insensitive substring of the ALSA/PulseAudio device name. Empty = system default.   |
| `audio_format`    | `flac`               | `flac` halves payload vs raw WAV.                                                         |
| `trigger`         | `key:ctrl+g`         | Keyboard: `key:[mod+…+]<key>`. Multiple modifiers: `key:ctrl+shift+g`. Modifiers: `ctrl`, `shift`, `alt`, `super`/`meta` (Meta key). Keys: `a`–`z`, `0`–`9`, `f1`–`f12`, `space`, `tab`, `escape`, arrow keys, etc. Mouse: `mouse:<button>` — `left`, `right`, `middle`, `x1` (back), `x2` (forward). |
| `one_key_trigger` | empty                | Optional single-key PTT (`key:ctrl`, `key:alt`, `key:f1`–`key:f12`). Foreign key mid-record cancels the take. |
| `auto_paste`      | `true`               | Save clipboard → set transcript → `Ctrl+V` → restore clipboard.                          |
| `paste_delay_ms`  | `50`                 | Applied before injecting `Ctrl+V` and before restoring the clipboard.                    |
| `request_timeout_s` | `30`               | HTTP POST timeout in seconds.                                                             |
| `smart_space`     | `true`               | Prepends a space when pasting within `continuation_window_s` of the last paste.          |
| `continuation_window_s` | `30`         | Seconds after the last paste that counts as a continuation.                               |
| `max_record_s`    | `300`                | Auto-stop recording after this many seconds (tray mode). `0` = off.                      |

Config search order:

1. `--config PATH` CLI argument
2. `<exe directory>/config.toml`
3. `$XDG_CONFIG_HOME/ghostscribe/config.toml`
4. `~/.config/ghostscribe/config.toml`
5. `./config.toml` (current working directory)

## 3. Run

```sh
./ghostscribe-client
```

### Detached mode (recommended for daily use)

Pass `--detach` to re-spawn as a background process with no terminal
attachment. Logs are written to
`$XDG_CONFIG_HOME/ghostscribe/ghostscribe.log`:

```sh
./ghostscribe-client --detach
# ghostscribe-client detached (pid 12345)
# logs: /home/you/.config/ghostscribe/ghostscribe.log
```

### Tray mode

Pass `--tray` to run with a system-tray icon (requires
`libappindicator3` or `libayatana-appindicator3`) and live config
editing. Implies `--detach`:

```sh
./ghostscribe-client --tray
```

Left-click the tray icon for the context menu:

- **Edit config…** – opens `config.toml` in `xdg-open`. Seeds a
  commented template if the file does not exist yet.
- **Reveal config** – opens the config directory in the file manager.
- **Reload now** – force-revalidates the config on demand.
- **Show log** – opens the log file in `xdg-open`.
- **Restart client** – respawns a fresh detached child and exits.
- **About GhostScribe** – version, config path, server URL.
- **Quit** – exit.

**Icon colour reflects state:** idle (grey), recording (red),
uploading (blue), error (amber).

**Live config reload.** A watcher thread stats the config file once per
second:

- **Hot keys** (`server_url`, `endpoint`, `auth_token`, `auto_paste`,
  `paste_delay_ms`, `request_timeout_s`, `smart_space`,
  `continuation_window_s`, `max_record_s`) are swapped in atomically.
- **Cold keys** (`trigger`, `one_key_trigger`, `input_device`,
  `audio_format`) require a restart (icon turns amber).

**Automatic recording limits (tray mode only).**

- **Auto-chunk** – partial upload every 2 minutes while recording.
- **`max_record_s`** – force-stops recording after N seconds. Set to
  `0` to disable.

Both timers are inactive in headless (`--detach`) mode.

### Foreground mode (for debugging)

Run without `--detach` to keep the terminal attached and see logs live:

```
GhostScribe client (headless) -> http://SERVER_HOST:5005/v1/auto
config:   /home/you/.config/ghostscribe/config.toml
trigger:  key:ctrl+g
one_key:  off
format:   flac
auth:     on
paste:    on (delay 50 ms)
Hold key:ctrl+g and speak. Release to transcribe. Ctrl+C to quit.
```

## 4. Prerequisites

- Linux with X11 and XRecord extension enabled (standard on all desktop
  installs). Wayland global hotkeys are not natively supported; XWayland
  works for most applications.
- Default microphone configured in PulseAudio / PipeWire.
- System libraries: `libappindicator3` (or `libayatana-appindicator3`),
  `libgtk-3`, `libxtst`, `libx11`.
- Network reachability to the server on port 5005.

No root access is needed. The XRecord-based global hook, microphone
access, and outbound HTTP all work from a standard user account.

## 5. What this client does and does not do

**Implemented:**

- Configurable trigger via `rdev::listen` (XRecord): keyboard chord,
  one-key PTT (modifier or F-key), or mouse button.
- Mouse PTT: `mouse:left/right/middle/x1/x2`.
- Microphone capture via `cpal` (ALSA/PulseAudio), downmixed to mono,
  resampled to 16 kHz.
- FLAC or WAV encoding.
- Multipart HTTP(S) POST via `ureq`, optional `X-Auth-Token`.
- Save-Paste-Restore clipboard injection via `arboard` + `rdev::simulate`.
- `--detach` mode: new process group, stdio to log file.
- `--tray` mode: state-tinted icon, live config reload, auto-chunk
  uploads, `max_record_s` cap.

**Not implemented:**

- No installer or `.desktop` autostart entry. Add the binary to
  `~/.config/autostart/` manually.
- No client-side VAD (server-side VAD handles silence).
- No streaming / live partial transcripts.
- Wayland native global hotkeys (XWayland fallback works for most apps).
- F13–F24 one-key support (rdev 0.5 maps them to `Unknown(keysym)`; TODO).
- No clipboard-change verification before restore.

## 6. Files in this folder

| File                  | Purpose                                                    |
| --------------------- | ---------------------------------------------------------- |
| `Cargo.toml`          | Rust package manifest.                                     |
| `src/lib.rs`          | Library entry point; re-exports modules.                   |
| `src/main.rs`         | Entry point; spawns hook thread + upload workers.          |
| `src/config.rs`       | TOML config loader (exe-dir, XDG, CWD).                   |
| `src/audio.rs`        | `cpal` capture, downmix, resample, FLAC/WAV encode.        |
| `src/hotkey.rs`       | `rdev::listen` hook; chord, one-key, and mouse-button PTT. |
| `src/upload.rs`       | Multipart `ureq` POST with `X-Auth-Token`.                 |
| `src/paste.rs`        | Save-Paste-Restore via `arboard` + `rdev::simulate`.       |
| `src/tray.rs`         | `--tray` icon, state-tinted glyphs, menu wiring.           |
| `src/watcher.rs`      | 1 s mtime poll of the active `config.toml`.                |
| `BUILD.md`            | Build instructions.                                        |
