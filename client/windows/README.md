# GhostScribe - Windows 11 client (Rust)

A single-file push-to-talk client for Windows 11, written in Rust and
shipped as one self-contained `ghostscribe-client.exe`. No Python, no
installer, no Visual C++ redistributable, no PortAudio DLL.

Behaviour is identical to the Linux client, except the trigger is
**Ctrl + G** instead of a mouse side button:

- Hold `Ctrl` **and** `G` -> microphone starts recording.
- Release **either** key -> buffer is encoded as 16 kHz mono WAV and
  POSTed to the configured `<server_url><endpoint>`.
- Transcript text goes to stdout; status/timing goes to stderr.
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
server_url   = "http://SERVER_HOST:5005"
endpoint     = "/v1/auto"         # or /v1/en or /v1/sk
auth_token   = ""                 # same value as the Linux client
input_device = ""                 # empty = Windows default mic
```

Config search order:

1. `--config PATH` CLI argument
2. `<exe folder>\config.toml`
3. `%APPDATA%\ghostscribe\config.toml`
4. `.\config.toml` (current working directory)

## 3. Run

Double-click the `.exe`, or from PowerShell:

```powershell
.\ghostscribe-client.exe
```

Banner:

```
GhostScribe client -> http://SERVER_HOST:5005/v1/auto
config:   C:\...\config.toml
trigger:  key:ctrl+g
auth:     on
device:   Microphone (Realtek Audio) (48000 Hz, 2 ch)
Hold Ctrl+G and speak. Release to transcribe. Ctrl+C to quit.
```

Hold `Ctrl+G`, speak, release either key:

```
[rec] ...
[rec] stopped, 112 kB raw
[recv] 58 kB in 420 ms (lang=en p=0.99)
[recv] transcript:
Hello, this is a test transcription.
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

- The `Ctrl+G` hotkey will not fire while a window running **as
  Administrator** has focus (Task Manager, admin-elevated PowerShell,
  UAC prompts, some installers). Focus a normal window and it works
  again.
- Global hooks are not visible inside an RDP session's host. Run the
  client locally, not through Remote Desktop.
- Corporate EDR (Defender for Endpoint, CrowdStrike, SentinelOne) can
  silently block `SetWindowsHookEx`. Symptom: banner prints but the
  hotkey does nothing. Only IT can allowlist the binary.

## 6. What this client intentionally does NOT do

Same scope as the original Python/Windows plan (see
`README.python.md` for the deferred-feature roadmap):

- No clipboard paste / `Ctrl+V` injection.
- No tray icon, no autostart.
- No client-side VAD.
- No streaming / live partials.

Transcripts go to stdout so you can pipe them wherever you like.

## 7. Files in this folder

| File                   | Purpose                                          |
| ---------------------- | ------------------------------------------------ |
| `Cargo.toml`           | Rust package manifest.                           |
| `.cargo/config.toml`   | Cross-compile defaults for `x86_64-pc-windows-gnu` with static CRT. |
| `src/main.rs`          | Entry point; spawns hook thread + upload workers. |
| `src/config.rs`        | TOML config loader (exe-dir, `%APPDATA%`, CWD).  |
| `src/audio.rs`         | `cpal` capture, downmix, resample, WAV encode.   |
| `src/hotkey.rs`        | `WH_KEYBOARD_LL` hook detecting `Ctrl+G`.        |
| `src/upload.rs`        | Multipart `ureq` POST with `X-Auth-Token`.       |
| `config.example.toml`  | Template config.                                 |
| `BUILD.md`             | Build instructions (Linux cross-compile & native).|
| `README.python.md`     | Earlier deferred Python-based design + deferred-feature roadmap (kept for reference). |
