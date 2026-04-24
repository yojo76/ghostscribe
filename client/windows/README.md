# GhostScribe - Windows 11 client (Rust)

A single-file push-to-talk client for Windows 10/11, written in Rust and
shipped as one self-contained `ghostscribe-client.exe`. No Python, no
installer, no Visual C++ redistributable, no PortAudio DLL.

- Hold the configured trigger (default `Ctrl+G`) -> microphone starts recording.
- Release any trigger key -> buffer is downmixed to mono, resampled to
  16 kHz, encoded as **FLAC** (or WAV if configured) and POSTed to
  the configured `<server_url><endpoint>`.
- Optional `one_key_trigger` (e.g. `key:ctrl`) lets a single modifier or
  F-key act as PTT; pressing a foreign key mid-take cancels without sending.
- Transcript text goes to stdout; status/timing goes to stderr.
- If `auto_paste = true` (the default), the transcript is copied to
  the clipboard, `Ctrl+V` is synthesised into the focused window, and
  the original clipboard contents are restored afterwards.
- Same `auth_token` (sent as `X-Auth-Token`) the Linux client uses.

## 1. Get the binary

Either:

- Grab the prebuilt `ghostscribe-client.exe` from a teammate, **or**
- Build it yourself (see `BUILD.md`). From a Linux host:
  ```bash
  sudo apt install -y mingw-w64
  rustup target add x86_64-pc-windows-gnu
  cd client/windows
  cargo build --release --target x86_64-pc-windows-gnu
  ```
  Output: `target/x86_64-pc-windows-gnu/release/ghostscribe-client.exe`.

## 2. Configure

Next to `ghostscribe-client.exe`, drop a `config.toml`. Start from
`config.example.toml` in this folder:

```toml
server_url      = "http://SERVER_HOST:5005"
endpoint        = "/v1/auto"       # /v1/auto | /v1/en
auth_token      = ""               # same value as the Linux client
input_device    = ""               # empty = Windows default mic
audio_format    = "flac"           # "flac" (smaller) or "wav"
trigger         = "key:ctrl+g"     # key:[modifier+]<keyname>  OR  mouse:<button>
                                   # mouse buttons: left, right, middle, x1 (back), x2 (forward)
one_key_trigger = ""               # empty, or key:ctrl|alt|f1..f24
auto_paste      = true             # false => stdout only
paste_delay_ms  = 50               # ms before Ctrl+V and before restore
```

All fields are optional; anything you omit falls back to the defaults
above.

| Key              | Default             | Notes                                                                                   |
| ---------------- | ------------------- | --------------------------------------------------------------------------------------- |
| `server_url`     | `http://localhost:5005` | No trailing slash.                                                                      |
| `endpoint`       | `/v1/auto`          | `/v1/en` forces English transcription.                                                   |
| `auth_token`     | empty               | Sent as `X-Auth-Token` when non-empty. **Do not commit your real token.**               |
| `input_device`   | empty               | Case-insensitive substring of the mic's Windows name. Empty = system default.           |
| `audio_format`   | `flac`              | `flac` halves payload vs raw WAV. Use `wav` only if FLAC gives the server trouble.      |
| `trigger`        | `key:ctrl+g`        | Keyboard: `key:[mod+…+]<key>`. Multiple modifiers: `key:ctrl+shift+g`. Modifiers: `ctrl`, `shift`, `alt`, `super`/`win` (Windows key). Keys: `a`–`z`, `0`–`9`, `f1`–`f24`, `space`, `delete`, etc. Mouse: `mouse:<button>` — `left`, `right`, `middle`, `x1` (back), `x2` (forward). |
| `one_key_trigger`| empty               | Optional single-key PTT. Allowed: `key:ctrl`, `key:alt`, `key:f1`-`key:f24`. Pressing a foreign key mid-record cancels the take; keys from `trigger` are neutral. See note below. |
| `auto_paste`     | `true`              | If `true`: save clipboard -> set transcript -> `Ctrl+V` -> restore clipboard.            |
| `paste_delay_ms` | `50`                | Applied both before injecting `Ctrl+V` and before restoring the clipboard.              |

**One-key trigger semantics.** When `one_key_trigger` is set, the client
accepts two ways to record: the chord in `trigger`, or the single key in
`one_key_trigger`. First to fully engage wins; the other path is inert for
the duration of that take. Press alone -> record; release alone -> send.
Press any other key while recording via one-key -> cancel the take (no
upload) and lock out until the one-key is released. Keys that are part of
the configured `trigger` (main key and modifier) are treated as neutral
and never cancel. Shift, letters, and digits are rejected as one-key
triggers because they would hijack normal typing.

Config search order:

1. `--config PATH` CLI argument
2. `<exe folder>\config.toml`
3. `%APPDATA%\ghostscribe\config.toml`
4. `.\config.toml` (current working directory)

**Never commit a filled-in `dist/config.toml`**. It contains your
`auth_token`. The repo's `.gitignore` explicitly excludes
`client/windows/dist/config.toml` for this reason; only
`config.example.toml` should live in git.

## 3. Run

Double-click the `.exe`, or from PowerShell:

```powershell
.\ghostscribe-client.exe
```

### Detached mode (recommended for daily use)

Pass `--detach` to re-spawn the client as a background process with no
console attachment. The original invocation prints the new PID and exits;
the child writes all log output to `%APPDATA%\ghostscribe\ghostscribe.log`:

```powershell
.\ghostscribe-client.exe --detach
# ghostscribe-client detached (pid 12345)
# logs: C:\Users\you\AppData\Roaming\ghostscribe\ghostscribe.log
```

Use `--detach` when:

- Launching from an **IDE-integrated terminal** (Cursor, VS Code, JetBrains)
  whose own UI is the paste target. A child of the IDE inherits a
  parent/foreground-window relationship that can cause Chromium-based
  text inputs (Cursor's agent chat being the canonical case) to silently
  ignore the synthesised `Ctrl+V`. A detached child sits outside that
  process tree and pastes reliably.
- Setting up **autostart-on-login**: create a Windows Startup shortcut
  pointing at `ghostscribe-client.exe --detach` and you get a
  console-less, log-to-file daemon that survives shell restarts.

Stop a detached instance with `Stop-Process -Name ghostscribe-client`.

### Tray mode (recommended for interactive editing)

Pass `--tray` to run with a system-tray icon and live config editing:

```powershell
.\ghostscribe-client.exe --tray
# ghostscribe-client detached (pid 12345)
# logs: C:\Users\you\AppData\Roaming\ghostscribe\ghostscribe.log
```

`--tray` implies `--detach`: the parent exits and the tray becomes the
only UI surface (no console window to manage). Left-click the icon to
open the context menu:

- **Edit config…** – opens the active `config.toml` in your default
  editor via `ShellExecuteW`. If no file exists yet, the client seeds
  `%APPDATA%\ghostscribe\config.toml` with a commented template first.
- **Reveal config in Explorer** – opens the parent folder with the file
  selected (`explorer.exe /select,…`).
- **Reload now** – force-revalidates the config file on demand, without
  waiting for the 1 s mtime poll.
- **Show log** – opens `%APPDATA%\ghostscribe\ghostscribe.log`.
- **Restart client** – respawns a fresh detached tray child and exits
  the current one. Use after changing a cold key (see below).
- **About GhostScribe** – version, config path, server URL.
- **Quit** – exit the tray session.

**Icon colour reflects state.** A single 32×32 filled-circle glyph,
tinted per state: idle (neutral grey), recording (red), uploading
(blue), error (amber). Hover the icon to see the full tooltip, e.g.
`GhostScribe — uploading… — last: 47 chars`.

**Live config reload.** A watcher thread stats the config file once per
second. When the file's `mtime` advances, the new contents are parsed
and diffed against the running config:

- **Hot keys** (`server_url`, `endpoint`, `auth_token`, `auto_paste`,
  `paste_delay_ms`, `request_timeout_s`, `smart_space`,
  `continuation_window_s`, `max_record_s`) are swapped in atomically
  under an `ArcSwap`. The next upload/paste sees the new value.
  Tooltip updates to `reloaded: server_url, auto_paste`.
- **Cold keys** (`trigger`, `one_key_trigger`, `input_device`,
  `audio_format`) cannot be changed mid-session because the audio stream
  and the low-level keyboard hook capture them at startup. The icon
  turns amber and the tooltip shows `restart required: trigger,
  audio_format`. Pick **Restart client** to apply them.
- **Parse errors** (malformed TOML, invalid keys, etc.) pop a modal
  message box with the diagnostic. The running config is untouched,
  so the client keeps working with the previous values.

**Automatic recording limits (tray mode only).**
While recording, two background timers run:

- **Auto-chunk** – every 2 minutes, the current buffer is checkpointed
  and a partial upload is sent so long transcripts reach the server
  incrementally.
- **`max_record_s`** (default 300 s) – if the trigger is still held
  after this many seconds, recording is force-stopped and the buffer
  is uploaded. Set to `0` to disable.

Both timers are inactive in headless (`--detach`) mode.

**What `--tray` does not do:**

- No balloon toasts (`Shell_NotifyIconW NIF_INFO`). Tooltips and the
  error dialog carry the same information.
- No taskbar icon; tray-only by design.
- No global hotkey to toggle the tray — right-click the icon and pick
  Quit to exit.

### Foreground mode (for debugging)

Run without `--detach` to keep the console attached and see logs live:

Banner:

```
GhostScribe client -> http://SERVER_HOST:5005/v1/auto
config:   C:\...\config.toml
trigger:  key:ctrl+g
one_key:  off
format:   flac
auth:     on
paste:    on (delay 50 ms)
device:   Microphone (Realtek Audio) (48000 Hz, 2 ch)
Hold key:ctrl+g and speak. Release to transcribe. Ctrl+C to quit.
```

Hold `Ctrl+G`, speak, release either key:

```
[rec] ...
[rec] stopped, 112 kB raw
[send] 58 kB flac
[recv] 58 kB in 420 ms (lang=en p=0.99)
[recv] transcript:
Hello, this is a test transcription.
[paste] done
```

## 4. Prerequisites on the Windows machine

- Windows 10 or 11, 64-bit.
- Default microphone configured in Settings -> System -> Sound -> Input.
- Microphone privacy enabled for desktop apps (Settings -> Privacy &
  security -> Microphone -> both toggles on).
- Network reachability to the server on port 5005:
  ```powershell
  curl.exe http://SERVER_HOST:5005/v1/health
  ```

No admin rights are needed. The low-level keyboard hook, microphone
access, and outbound HTTP all work from a standard user account.

## 5. What does NOT work without admin

Same UIPI caveat as any Windows input hook:

- The configured hotkey will not fire while a window running **as
  Administrator** has focus (Task Manager, admin-elevated PowerShell,
  UAC prompts, some installers). Focus a normal window and it works
  again.
- Global hooks are not visible inside an RDP session's host. Run the
  client locally, not through Remote Desktop.
- Corporate EDR (Defender for Endpoint, CrowdStrike, SentinelOne) can
  silently block `SetWindowsHookEx`. Symptom: banner prints but the
  hotkey does nothing. Only IT can allowlist the binary.

## 6. What this client does and does not do

**Implemented:**

- Configurable trigger (`trigger`) via `WH_KEYBOARD_LL` (keyboard chord)
  or `WH_MOUSE_LL` (mouse button), both unprivileged.
- Optional single-key PTT (`one_key_trigger`): press alone to record,
  release to send; foreign key mid-take cancels the recording.
- Microphone capture via `cpal` (WASAPI under the hood), any source
  rate / channels, downmixed to mono and resampled to 16 kHz.
- FLAC or WAV encoding of the capture buffer.
- Multipart HTTP(S) POST via `ureq`, optional `X-Auth-Token`.
- Save-Paste-Restore clipboard injection (controlled by `auto_paste`):
  save current clipboard -> set transcript + trailing space ->
  `Ctrl+V` via `SendInput` -> wait `paste_delay_ms` -> restore.
- `--detach` mode: re-spawns as a background process with stdio
  redirected to `%APPDATA%\ghostscribe\ghostscribe.log`.
- `--tray` mode: system-tray icon with state-aware colours, live config
  reload (hot keys applied atomically; cold keys surface a
  restart-required state), and an editor-launching menu.

**Not implemented (deferred; see `README.python.md` for the roadmap):**

- No installer. Autostart-on-login works today by pointing a Windows
  Startup shortcut at `ghostscribe-client.exe --tray`.
- No terminal detection / bracketed-paste fallback.
- No client-side VAD (server-side VAD handles silence).
- No streaming / live partial transcripts.
- No clipboard-change verification before restore (if you copy
  something between the paste and the restore, the restore will
  clobber it).

Transcripts always go to **stdout** regardless of `auto_paste`, so you
can pipe them wherever you like even when the paste is on.

## 7. Files in this folder

| File                   | Purpose                                          |
| ---------------------- | ------------------------------------------------ |
| `Cargo.toml`           | Rust package manifest (bin + lib targets).       |
| `.cargo/config.toml`   | Cross-compile defaults for `x86_64-pc-windows-gnu` with static CRT. |
| `src/lib.rs`           | Library entry point; re-exports modules for integration tests. |
| `src/main.rs`          | Entry point; spawns hook thread + upload workers. |
| `src/config.rs`        | TOML config loader (exe-dir, `%APPDATA%`, CWD).  |
| `src/audio.rs`         | `cpal` capture, downmix, resample, FLAC/WAV encode. |
| `src/hotkey.rs`        | `WH_KEYBOARD_LL` + `WH_MOUSE_LL` hooks; chord, one-key, and mouse-button PTT. |
| `src/upload.rs`        | Multipart `ureq` POST with `X-Auth-Token`.       |
| `src/paste.rs`         | Save-Paste-Restore via Win32 clipboard + `SendInput`. |
| `src/tray.rs`          | `--tray` icon, procedural state-tinted glyphs, menu wiring. |
| `src/watcher.rs`       | 1 s mtime poll of the active `config.toml`; diffs against live config. |
| `tests/upload.rs`      | Integration tests against a mock HTTP server.    |
| `tests/config_diff.rs` | Integration tests for hot/cold `config::diff` classification. |
| `config.example.toml`  | Template config (safe to commit).                |
| `dist/ghostscribe-client.exe` | Prebuilt release binary.                   |
| `dist/config.toml`     | **Gitignored.** Your local config with a real `auth_token`. |
| `BUILD.md`             | Build instructions (Linux cross-compile & native).|
| `README.python.md`     | Earlier deferred Python-based design (kept for reference). |
