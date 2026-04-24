# Building the GhostScribe Linux client

The client is a single self-contained `ghostscribe-client` binary (~2.7 MB).
No Python, no virtualenv, no system daemon required.

## System dependencies

```bash
sudo apt install \
    libayatana-appindicator3-dev \
    libgtk-3-dev \
    libx11-dev \
    libxtst-dev \
    libxdo-dev
```

> **Ubuntu/Mint 22+** ship the Ayatana fork of appindicator. Use
> `libayatana-appindicator3-dev` (not the older `libappindicator3-dev`,
> which conflicts). The `tray-icon` crate detects which variant is
> present at build time via pkg-config.

## Build

```bash
cd client/linux-rs
cargo build --release
```

The output is at:

```
client/linux-rs/target/release/ghostscribe-client
```

Copy this binary and a `config.toml` (start from `dist/config.example.toml`)
anywhere and run it.

## Refresh the prebuilt binary in `dist/`

```bash
cp target/release/ghostscribe-client dist/ghostscribe-client
```

## Running

```bash
# Tray mode (recommended): spawns a background process with a tray icon.
./ghostscribe-client --tray

# Foreground / headless (no tray, logs to stderr).
./ghostscribe-client

# Custom config path.
./ghostscribe-client --config /path/to/config.toml
```

Log file (tray mode): `~/.config/ghostscribe/ghostscribe.log`

## Config search order

1. `--config PATH`
2. `<exe directory>/config.toml`
3. `$XDG_CONFIG_HOME/ghostscribe/config.toml`
4. `~/.config/ghostscribe/config.toml`
5. `./config.toml`

## What it does

1. Opens the default microphone (or `input_device` from config) via ALSA/PulseAudio through CPAL.
2. Listens for the configured `trigger` key chord globally via `rdev` (X11 XRecord extension — no root required on a normal desktop).
3. While the trigger is held, samples are appended to an in-memory buffer.
4. On release, the buffer is downmixed to mono, resampled to 16 kHz, encoded as **FLAC** (default) or WAV, and POSTed to `<server_url><endpoint>` as multipart `audio=...`.
5. `X-Auth-Token` header carries `auth_token` from `config.toml` if present.
6. If `auto_paste = true` (default), the transcript is pushed onto the clipboard, `Ctrl+V` is injected via `rdev::simulate` (XTest) into the focused window, and the previous clipboard content is restored after `paste_delay_ms`.
7. Auto-chunk: every 2 minutes of continuous recording a partial upload is triggered; transcripts are appended as the recording continues.
8. Max duration: if `max_record_s` is set, recording stops automatically after that many seconds.
9. Transcript text goes to **stdout**; status/timings go to **stderr** (or the log file in tray mode).

## Known platform constraints

- **X11 only**: `rdev::listen` uses the X11 XRecord extension; Wayland is not yet supported for global key listening. Under XWayland (the default on Ubuntu 22+/Mint 21+) this works transparently.
- **XTest for injection**: `rdev::simulate` uses the X11 XTest extension. Root is not required; the extension must not be disabled in `xorg.conf`.
- **Clipboard**: `arboard` maintains the X11 clipboard selection in a background thread so the content survives the function returning. Under Wayland (without XWayland) the clipboard may not persist.
