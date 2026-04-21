# GhostScribe — Minimal Windows Client

A dead-simple Python 3 push-to-talk client for Windows. Hold a key,
speak, release, transcript prints to stdout. That's it.

No clipboard injection. No paste. No save/restore. No system tray. No
installer. One script, one config, one command.

This document is deliberately verbose so a first-time setup goes right
on the first try.

---

## 0. No admin rights? No problem.

Everything in this guide works on a standard Windows user account with
**no administrator privileges**. You do not need UAC, you do not need
IT to install anything for you, you do not need to touch any system
policy. Specifically:

- Python installs per-user (`Install for me only`). PATH edits go to
  `HKCU` only, no UAC prompt.
- `pip install` into a venv writes only under your user folder.
- `pynput`'s low-level keyboard hook works from an unprivileged process
  (it only needs a message loop, which Python has).
- `sounddevice` / microphone access is a per-user Windows privacy
  setting, no admin.
- Outbound HTTP to the server needs no firewall change (Windows
  Firewall only prompts on *inbound* listeners).

There are exactly **two** things a no-admin user can't work around.
Both are covered in the Troubleshooting section, but flagging them
here:

1. **UIPI (User Interface Privilege Isolation).** Your PTT key will
   **not** fire while a window running as Administrator has focus
   (Task Manager, admin-launched PowerShell, UAC prompts, some
   installers, a handful of games). As soon as a normal window has
   focus again, it works. No fix without admin. In a locked-down
   environment this is usually irrelevant because you can't elevate
   anything anyway.
2. **Corporate AV / EDR** (Defender for Endpoint, CrowdStrike, SentinelOne, etc.)
   can silently refuse to install low-level keyboard hooks. Symptom:
   client starts cleanly, banner prints, but the PTT key does nothing
   and no error appears. Only IT can allowlist `python.exe`.

---

## 1. Prerequisites

- **Windows 10 or 11**, 64-bit.
- **Python 3.11 or 3.12**, 64-bit, from [python.org](https://www.python.org/downloads/windows/).
  - In the installer, pick **"Install for me only"** (or "Customize
    installation" -> untick "Install for all users").
  - Tick **"Add python.exe to PATH"**.
  - Python 3.10 works but pulls in a `tomli` fallback; 3.13 sometimes
    lags a week or two behind `sounddevice` wheel releases. 3.11 or 3.12
    are the safest bets today.
  - Verify in a **new** PowerShell window:
    ```powershell
    python --version
    ```
- A working **microphone** selected as the default recording device
  (Settings -> System -> Sound -> Input).
- **Microphone privacy** turned on:
  Settings -> Privacy & security -> Microphone ->
  both "Let apps access your microphone" *and* "Let desktop apps
  access your microphone" set to **On**.
- **Network reachability** to the GhostScribe server:
  ```powershell
  curl.exe http://SERVER_HOST:5005/v1/health
  ```
  Expect JSON with `"ready": true`. If `ready` is `false`, the server
  is still warming up; wait 10-30 seconds.

You do **not** need PortAudio separately; the `sounddevice` wheel
bundles it. You do **not** need Microsoft Visual C++ Build Tools; every
dependency ships a binary wheel for Windows x64.

---

## 2. Audio specs (what the server expects)

Match exactly:

| Property      | Value                           |
| ------------- | ------------------------------- |
| Sample rate   | 16 000 Hz                       |
| Channels      | 1 (mono)                        |
| Sample width  | 16-bit signed PCM (`int16`)     |
| Container     | WAV (RIFF, PCM, no compression) |
| Field name    | `audio` (multipart form POST)   |
| Content-Type  | `audio/wav`                     |
| Max size      | ~25 MB (about 13 minutes)       |

The server runs VAD (voice-activity detection) on its side, so short
leading/trailing silence is automatically trimmed -- don't worry about
exact timing.

---

## 3. PTT key -- what to pick

`pynput` can hook almost any key globally. Good choices, ranked by
"unlikely to conflict with anything you use":

1. **`f13`-`f24`** -- true blank keys on most keyboards. If yours has
   them (or you map one via PowerToys / AutoHotkey), the cleanest
   option.
2. **`pause`** -- the Pause/Break key. Nobody uses it. Very safe.
3. **`ctrl_r`** -- Right Ctrl. The default. Rarely bound in apps.
4. **`menu`** -- the "application" key next to Right Ctrl.
5. **`caps_lock`** -- works, but Windows still toggles the caps state.

Avoid: `space`, `enter`, letters, digits, `alt_l`, `ctrl_l`,
`shift_*`. Those fire inside every app you use.

Names the client understands (from `pynput.keyboard.Key`):

```
alt_gr, alt_l, alt_r, backspace, caps_lock, cmd, cmd_l, cmd_r,
ctrl_l, ctrl_r, delete, down, end, enter, esc,
f1, f2, ... f24,
home, insert, left, menu, num_lock, page_down, page_up,
pause, print_screen, right, scroll_lock, shift_l, shift_r,
space, tab, up
```

Single printable character keys also work, e.g.
`ptt_key = "\`"` for backtick.

Mouse Button 8/9 ("back"/"forward" side buttons) are **not** wired up
in this client. See section 9.

---

## 4. Install

Open **PowerShell** (not `cmd.exe`; PowerShell handles venv activation
better). Install into your user folder -- **do not** pick `C:\tools`
or `C:\Program Files`; those usually need admin:

```powershell
cd $env:USERPROFILE
git clone https://github.com/your-org/ghostscribe.git
cd ghostscribe\client

python -m venv .venv
.\.venv\Scripts\Activate.ps1

python -m pip install --upgrade pip
pip install -r requirements.txt
```

If PowerShell refuses to run `Activate.ps1` with an execution-policy
error, run this once (no admin needed -- `CurrentUser` scope is
unprivileged):

```powershell
Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned
```

No Git? Either install it per-user via `winget install --scope user Git.Git`,
or download the repo as a ZIP from GitHub and extract it under
`%USERPROFILE%\ghostscribe`.

The dependencies are:

- `sounddevice` -- mic capture (bundles PortAudio).
- `soundfile` -- WAV encoding (bundles libsndfile).
- `numpy` -- audio buffer math.
- `pynput` -- global keyboard hook.
- `httpx` -- HTTP POST.

---

## 5. Configure

```powershell
copy config.example.toml config.toml
notepad config.toml
```

Edit these five lines:

```toml
server_url   = "http://SERVER_HOST:5005"   # e.g. http://192.168.1.50:5005
endpoint     = "/v1/auto"                  # or /v1/en
ptt_key      = "ctrl_r"                    # see section 3
auth_token   = ""                          # only if server requires it
input_device = ""                          # empty = Windows default mic
```

To pin a specific microphone instead of the default, list devices:

```powershell
python -c "import sounddevice; print(sounddevice.query_devices())"
```

Sample output:

```
   0 Microsoft Sound Mapper - Input, MME (2 in, 0 out)
>  1 Microphone (Realtek Audio), MME (2 in, 0 out)
   2 Microphone Array (Intel Smart Sound), MME (2 in, 0 out)
```

Then set either the integer index (`input_device = "1"`) or the exact
name (`input_device = "Microphone (Realtek Audio)"`). Leave empty
unless the default is wrong.

---

## 6. Run

```powershell
python -m ghostscribe_client
```

Expected banner:

```
GhostScribe client -> http://SERVER_HOST:5005/v1/auto
config:   C:\Users\you\ghostscribe\client\config.toml
ptt key:  ctrl_r
device:   (system default)
auth:     off
Hold the PTT key and speak. Release to transcribe. Ctrl+C to quit.
```

**Hold Right Ctrl, speak, release.** Expected output:

```
[rec] ...
[rec] stopped, 112 kB
[recv] 112 kB in 430 ms (lang=en p=0.99)
Hello, this is a test transcription.
```

**stdout** gets only the transcript (pipe-friendly).
**stderr** gets status, timing, and errors.

One-shot overrides without editing the config:

```powershell
python -m ghostscribe_client --endpoint /v1/en
python -m ghostscribe_client --server-url http://192.168.1.50:5005
python -m ghostscribe_client --ptt-key f12
python -m ghostscribe_client --input-device "USB Mic"
```

---

## 7. End-to-end smoke test (skip the hotkey)

If the keyboard hook is misbehaving but you want to prove the
mic/server path works in isolation, record 3 s and POST it directly:

```powershell
python -c "import sounddevice as sd, soundfile as sf; sf.write('sample.wav', sd.rec(int(3*16000), samplerate=16000, channels=1, dtype='int16', blocking=True), 16000, subtype='PCM_16', format='WAV')"
curl.exe -F "audio=@sample.wav" http://SERVER_HOST:5005/v1/auto
```

If `curl.exe` returns a JSON transcript, the mic and server are fine
and any remaining issue is the keyboard hook or device selection in
section 5.

---

## 8. Troubleshooting

### `ModuleNotFoundError` / "No such module"

You're running system Python, not the venv's. Re-activate:

```powershell
cd $env:USERPROFILE\ghostscribe\client
.\.venv\Scripts\Activate.ps1
```

The prompt should start with `(.venv)`.

### `PortAudioError: Error querying device` / "Error opening InputStream"

Default mic is misconfigured or held exclusively by another app.

1. Settings -> System -> Sound -> Input: pick a real device and
   confirm the input level meter moves when you speak.
2. Close Teams / Zoom / Discord / OBS briefly and retry -- some apps
   grab the mic in exclusive mode.
3. List devices and pin one explicitly (section 5).

### Nothing happens when I press the PTT key

- Confirm the client is still running (banner visible in the console).
- Try a different key. Right Ctrl, `pause`, `f13`-`f24` are the
  safest tries.
- **UIPI caveat:** the key will not fire while a window running *as
  Administrator* has focus (Task Manager, admin PowerShell, UAC
  prompts). Focus a normal window and retry.
- **Remote Desktop caveat:** global hooks do not see keys pressed in
  an RDP session's host. Run the client locally.
- **Corporate AV / EDR:** if nothing works and there are no errors,
  your security software is likely blocking the low-level hook. Only
  IT can allowlist `python.exe` or your venv's `python.exe`. Ask for:
  > "Please allowlist `%USERPROFILE%\ghostscribe\client\.venv\Scripts\python.exe`
  > for low-level keyboard hooks (SetWindowsHookEx WH_KEYBOARD_LL).
  > Used internally by a dictation tool; no keystrokes leave the
  > machine except to the internal GhostScribe server."

### HTTP 401 "invalid or missing X-Auth-Token"

Server has `GHOSTSCRIBE_AUTH_TOKEN` set. Put the same value in
`config.toml` under `auth_token`, or pass `--auth-token "..."`.

### HTTP 413 "audio payload ... exceeds limit"

You held the key for too long (~13 min at 16 kHz mono int16). Speak
in shorter bursts, or ask the server admin to raise
`GHOSTSCRIBE_MAX_UPLOAD_MB`.

### HTTP 503 "server is still warming up"

Server just started. `/v1/health` shows `"ready": false` until warm-up
finishes. Wait 10-30 s and retry.

### Transcript is empty

Usually one of:

- The PTT key was released before any audio was captured. Hold
  longer. The client prints the captured size before sending; under
  ~8 kB means you spoke for less than ~0.25 s.
- Mic is muted at the OS level. Check the mute icon in Sound
  settings.
- Server-side VAD trimmed everything as silence. Move closer to the
  mic or raise input volume.

### "I want to see exactly what's happening"

Run unbuffered so stderr flushes immediately:

```powershell
python -u -m ghostscribe_client
```

Server-side logs are far more detailed -- ask whoever runs the server
for the current `journalctl -u ghostscribe-server -f`.

---

## 9. What this client does NOT do (by design)

All of the items below are intentionally **out of scope** for this
client. They are listed here only so nobody wastes time asking "why
not": see section 10 for the correct implementation order.

- No **clipboard paste** into the focused app -- transcript goes to
  stdout only.
- No **Save-Paste-Restore** clipboard dance.
- No **mouse-button PTT** (Button 8/9).
- No **client-side VAD** (the server handles it).
- No **streaming** / live partial transcripts.
- No **tray icon**, autostart, or packaged `.exe`.
- No **window-specific behaviour** (terminal detection, bracketed
  paste, etc.).

---

## 10. Additional functionality (NICE-TO-HAVE, LATER ONLY)

> **READ THIS FIRST.** None of the features below should be
> implemented until the basic client above has been tested
> end-to-end against the real GhostScribe server on at least one
> Windows machine, and the server integration has been signed off
> as stable. Each of these items adds a new failure mode, a new
> debugging surface, or a new dependency on OS-level behaviour that
> is painful to diagnose *through* an unstable base. Do the boring
> version first, prove the pipeline, then add one item at a time.

Ordered by increasing risk / complexity. Stop at any point; each
stage is independently useful.

### 10.1 Console QoL (low risk)

- Coloured status lines on stderr (`colorama` auto-activates ANSI on
  Windows).
- A short "beep" on successful transcript (`winsound.MessageBeep`) and
  a different beep on HTTP error. Very cheap UX win.
- Persist the last transcript to a file for "paste it myself" workflows
  (`--save-last transcripts.log`). Append-only, no rotation needed
  initially.

### 10.2 Save-Paste-Restore clipboard injection (medium risk)

The real dictation experience. Must be designed carefully because this
is where most bugs live on Windows:

1. Back up current clipboard content (handle text + Unicode + empty
   clipboards + non-text formats like images -- treat those as "do
   not restore").
2. Put transcript onto clipboard.
3. Synthesise `Ctrl+V` via `pynput` or `SendInput` (via `ctypes`).
4. Wait a configurable "paste buffer" delay (50-150 ms).
5. Verify clipboard still holds *our* transcript before restoring
   (the user may have copied something mid-paste -- do not clobber).
6. Restore original clipboard content.

**Implement only after** the stdout client is proven. Add a
`--inject` / `--no-inject` flag so the stdout mode remains available
for debugging.

### 10.3 Mouse-button PTT (medium risk)

Side buttons (Button 8 / 9) on gaming and productivity mice. On
Windows, `pynput.mouse.Listener` handles extra buttons fine via
`Button.x1` / `Button.x2`. Design:

- Abstract a `PTTBackend` interface with `keyboard` and `mouse`
  implementations.
- Config: `ptt_source = "keyboard" | "mouse"` plus the key/button
  name.

### 10.4 Terminal detection + Ctrl+Shift+V (medium risk)

Detect if the focused window is a terminal (Windows Terminal,
ConHost, cmd, PowerShell) and use `Ctrl+Shift+V` (plus optionally
bracketed-paste sequences) instead of `Ctrl+V`. Use
`GetForegroundWindow` + `GetWindowText` + window class (e.g.
`CASCADIA_HOSTING_WINDOW_CLASS` for Windows Terminal) via `ctypes`.
Cache results per-hwnd for speed.

### 10.5 Tray icon & autostart (low-medium risk)

- `pystray` + a tiny PNG for a tray icon showing idle / recording /
  sending / error state.
- Autostart on login: drop a `.lnk` into
  `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\`. No admin
  needed. Do **not** use Task Scheduler unless you need elevated
  execution (you don't).

### 10.6 Packaging (medium risk)

- `pyinstaller --onefile --noconsole` for a single `.exe`. Biggest
  catch: `sounddevice` and `soundfile` need their bundled DLLs
  copied; use the `--collect-binaries` flag or a `.spec` file.
- Consider signing the resulting binary if your AV policy requires
  it. Without signing, SmartScreen will throw a warning on first run.

### 10.7 Client-side VAD (higher risk)

Add `silero-vad` (PyTorch) or `webrtcvad` (native) to trim silence
before upload. Saves bandwidth, but:

- PyTorch adds ~200 MB to the install.
- `webrtcvad` needs a C++ build toolchain unless you find a prebuilt
  wheel.
- The server already runs VAD, so benefits are marginal on a LAN.

Only worth it if you go remote / metered-network later.

### 10.8 Streaming / live partials (highest risk)

Upgrade the HTTP API to a WebSocket `/v1/stream` that sends audio
chunks as they arrive and emits partial hypotheses. Requires server
changes (chunked `faster-whisper` batching or a streaming wrapper like
`whisper_streaming`) and a new client event loop. Biggest latency
win for long dictations, but completely separate design from the
current release-to-send model. **Do not** start this until everything
above is solid.

---

## 11. Quick reference

| Thing                 | Value                                                               |
| --------------------- | ------------------------------------------------------------------- |
| Admin required?       | No                                                                  |
| Python                | 3.11 or 3.12, 64-bit, per-user install                              |
| Audio                 | 16 kHz mono 16-bit PCM WAV                                          |
| Default PTT key       | Right Ctrl (`ctrl_r`)                                               |
| Default endpoint      | `/v1/auto`                                                          |
| Default server port   | `5005`                                                              |
| Install location      | `%USERPROFILE%\ghostscribe`                                         |
| Config search order   | `--config` -> `~/.config/ghostscribe/config.toml` -> `.\config.toml` |
| Exit                  | Ctrl+C in the console                                               |
| stdout                | Transcript text only                                                |
| stderr                | Status, timing, errors                                              |
