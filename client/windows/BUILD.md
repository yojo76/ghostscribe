# Building the GhostScribe Windows client

The client is a single self-contained `ghostscribe-client.exe` (~1.7 MB).
No installer, no Visual C++ redistributable, no Python, no extra DLLs.
Only standard Windows system DLLs are linked (`kernel32`, `user32`,
`ole32`, `oleaut32`, `ws2_32`, `bcrypt`, `msvcrt`).

## Cross-compile from Linux (what this repo supports)

### One-time setup

```bash
sudo apt install -y mingw-w64
rustup target add x86_64-pc-windows-gnu
```

### Build

```bash
cd client/windows
cargo build --release --target x86_64-pc-windows-gnu
```

The output is at:

```
client/windows/target/x86_64-pc-windows-gnu/release/ghostscribe-client.exe
```

Copy this `.exe` and a `config.toml` (start from `config.example.toml`)
to any Windows 11 machine and double-click, or run from PowerShell.

## Native build on Windows

```powershell
rustup target add x86_64-pc-windows-msvc
cargo build --release
```

The MSVC target produces a comparable `.exe` that links against the
usual Windows 10/11 system DLLs.

## What it does

1. Opens the default microphone via WASAPI (built into Windows 10/11).
2. Installs a low-level keyboard hook (`SetWindowsHookEx` /
   `WH_KEYBOARD_LL`) so it sees the configured `trigger` (default
   `key:ctrl+g`) even when focus is elsewhere.
3. While the trigger is held, samples are appended to an in-memory
   buffer.
4. On release, the buffer is downmixed to mono, resampled to 16 kHz,
   encoded as **FLAC** (default) or WAV, and POSTed to
   `<server_url><endpoint>` as multipart `audio=...`.
5. `X-Auth-Token` header carries `auth_token` from `config.toml`
   if present.
6. If `auto_paste = true` (default), the transcript is pushed onto
   the Windows clipboard, `Ctrl+V` is synthesised via `SendInput`
   into the focused window, and the previous clipboard content is
   restored after `paste_delay_ms`.
7. Transcript text goes to **stdout**; status/timings go to **stderr**.

This mirrors the Linux client's `mouse:x2` trigger behaviour, only with
a configurable keyboard trigger (default `Ctrl+G`) instead of the mouse
side button.

## Known platform constraints

- **UIPI**: the hook will not fire while a window running as
  Administrator has focus (same limitation as the Python Windows
  client).
- **Corporate EDR**: low-level keyboard hooks may be blocked by
  Defender for Endpoint / CrowdStrike / SentinelOne. Allowlist
  `ghostscribe-client.exe` if the hotkey does nothing and no errors
  appear.
- **Remote Desktop**: global hooks do not see keys pressed in an RDP
  session's host. Run locally.
