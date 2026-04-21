# GhostScribe

Low-latency, intranet-only speech-to-text. A GPU-resident
`faster-whisper large-v3-turbo` server plus a minimal push-to-talk
Python client.

This is the **first commit**: server is production-shaped (FastAPI +
systemd), client is intentionally dumb (prints transcripts to stdout,
no clipboard injection yet).

See `start_requirements.md` for the product vision.

## Architecture

```mermaid
flowchart LR
    User[User holds PTT key] --> Client[ghostscribe_client]
    Client -->|records 16 kHz mono PCM| Buffer[In-memory WAV]
    Buffer -->|HTTP POST multipart| Server["FastAPI :5005"]
    Server --> Guard["size + soundfile sniff<br/>optional X-Auth-Token"]
    Guard --> Sem["asyncio.Semaphore(1)"]
    Sem --> Model["faster-whisper large-v3-turbo<br/>VAD on, beam=5"]
    Model -->|"text, language, probability"| Server
    Server -->|JSON| Client
    Client -->|stdout| User
```

## Repo layout

```
ghostscribe/
  server/                          FastAPI app + systemd unit
    ghostscribe_server/
    assets/silence_1s.wav          warm-up sample
    systemd/ghostscribe-server.service
    systemd/server.env.example
    pyproject.toml
    requirements.txt
  client/
    linux/                         minimal Linux PTT client (Python)
      ghostscribe_client/
      config.example.toml
      pyproject.toml
      requirements.txt
    windows/                       Windows PTT client (planned)
      README.md
  start_requirements.md            product spec
  HARDWARE.md                      reference host hardware
```

## API

All routes under `/v1/`. Responses are JSON `{text, language, language_probability}`.

| Method | Path          | Behaviour                                              |
| ------ | ------------- | ------------------------------------------------------ |
| GET    | `/v1/health`  | Liveness + readiness probe.                            |
| POST   | `/v1/en`      | English audio in -> English text.                      |
| POST   | `/v1/sk`      | Slovak audio in -> English text (Whisper `translate`). |
| POST   | `/v1/auto`    | Autodetect language, transcribe (no translation).      |

All POST endpoints accept a multipart form field named `audio`. If
`GHOSTSCRIBE_AUTH_TOKEN` is set on the server, clients must send an
`X-Auth-Token` header.

Swagger UI is at `http://HOST:5005/docs`.

## Server

### Prerequisites

- Linux (tested target: Linux Mint 21/22 with an X11 session).
- NVIDIA GPU with recent driver + CUDA runtime. RTX 5060 Ti (Blackwell, sm_120) requires **CUDA 12.8+** and a `ctranslate2` build that supports it; `faster-whisper >= 1.1.0` ships compatible wheels. If you see `no kernel image available`, upgrade `ctranslate2`.
- Python 3.10+.

### Install (manual, one-time)

```bash
sudo useradd --system --home /opt/ghostscribe --shell /usr/sbin/nologin ghostscribe
sudo mkdir -p /opt/ghostscribe /etc/ghostscribe
sudo chown -R ghostscribe:ghostscribe /opt/ghostscribe

sudo -u ghostscribe git clone https://github.com/your-org/ghostscribe.git /opt/ghostscribe

cd /opt/ghostscribe/server
sudo -u ghostscribe python3 -m venv .venv
sudo -u ghostscribe .venv/bin/pip install --upgrade pip
sudo -u ghostscribe .venv/bin/pip install -r requirements.txt
sudo -u ghostscribe mkdir -p logs

sudo cp systemd/server.env.example /etc/ghostscribe/server.env
sudo $EDITOR /etc/ghostscribe/server.env

sudo cp systemd/ghostscribe-server.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now ghostscribe-server
sudo systemctl status ghostscribe-server
journalctl -u ghostscribe-server -f
```

Give it up to ~30 s on first boot: the model needs to download and warm
up. `/v1/health` returns `"ready": true` once it's ready.

### Running without systemd (dev)

```bash
cd server
python -m venv .venv
. .venv/bin/activate
pip install -r requirements.txt
uvicorn ghostscribe_server.app:app --host 0.0.0.0 --port 5005 --workers 1
```

`--workers 1` is not optional: more workers would load the model N
times and blow out VRAM.

### Configuration

All env vars are optional. Defaults in parentheses:

| Variable                    | Default                                  |
| --------------------------- | ---------------------------------------- |
| `GHOSTSCRIBE_HOST`          | `0.0.0.0`                                |
| `GHOSTSCRIBE_PORT`          | `5005`                                   |
| `GHOSTSCRIBE_MODEL`         | `large-v3-turbo`                         |
| `GHOSTSCRIBE_DEVICE`        | `cuda`                                   |
| `GHOSTSCRIBE_COMPUTE_TYPE`  | `int8_float16`                           |
| `GHOSTSCRIBE_LOG_PATH`      | `./logs/ghostscribe_server.log`          |
| `GHOSTSCRIBE_MAX_UPLOAD_MB` | `25`                                     |
| `GHOSTSCRIBE_AUTH_TOKEN`    | *(unset -> auth disabled)*               |

### Smoke test

Record a short WAV (or use any `.wav`/`.flac`/`.ogg` file):

```bash
arecord -d 3 -f S16_LE -r 16000 -c 1 sample.wav   # Linux
curl -F "audio=@sample.wav" http://localhost:5005/v1/auto
curl http://localhost:5005/v1/health
```

## Clients

The repo hosts one client per platform under `client/`. They are
independent projects with their own `requirements.txt` / `pyproject.toml`
so they can be worked on separately.

- `client/linux/` -- reference Python client for X11 Linux (Cinnamon /
  GNOME on X11). Instructions below.
- `client/windows/` -- planned Windows client; design + setup notes in
  [client/windows/README.md](client/windows/README.md).

## Linux client

### Install

```bash
cd client/linux
python3 -m venv .venv
. .venv/bin/activate
pip install -r requirements.txt
cp config.example.toml config.toml
$EDITOR config.toml
```

On Linux you also need PortAudio for `sounddevice` and `xclip` for the
clipboard push:

```bash
sudo apt install libportaudio2 xclip
```

### Usage

```bash
python -m ghostscribe_client
```

Hold the configured trigger (default: mouse button `x2` -- the thumb
"forward" button). Speak. Release. The transcript is pushed onto the X11
CLIPBOARD via `xclip` and then `Ctrl+V` is simulated into whatever window
has focus. Timing and the full transcript also print to stderr:

```
GhostScribe client -> http://localhost:5005/v1/auto
config:   /home/me/.config/ghostscribe/config.toml
trigger:  mouse:x2
device:   (system default)
format:   flac
paste:    on (delay 50 ms)
auth:     off
Hold the trigger and speak. Release to transcribe. Ctrl+C to quit.
[rec] ...
[rec] stopped, 96 kB raw
[recv] 54 kB in 430 ms (lang=en p=0.99)
[paste] pasted into focused window:
Hello, this is a test transcription.
```

Trigger formats:

- `mouse:<button>` -- `left`, `middle`, `right`, `x1` / `back`,
  `x2` / `forward`, or `button8` / `button9`.
- `key:<name>` -- any pynput `keyboard.Key` name (`ctrl_r`, `f12`, ...)
  or a single character.
- `key:<mods>+<key>` -- chord, e.g. `key:ctrl+g`, `key:ctrl+shift+space`.
  Modifiers: `ctrl`, `shift`, `alt`, `super` (each matches both left
  and right variants). Releasing either the target or any modifier
  stops recording and sends.

CLI flags override config values: `--endpoint /v1/sk`, `--server-url ...`,
`--trigger key:ctrl+g`, `--auth-token ...`, `--input-device "USB Mic"`,
`--audio-format wav`, `--no-paste`, `--paste-delay-ms 100`.

### Notes / known limitations

- Global hooks (keyboard and mouse) work on **X11** (Linux Mint Cinnamon's
  default). On **Wayland** `pynput` cannot install a global hook; run
  the client in a terminal that has focus, or switch the session to X11.
- Auto-paste **overwrites the system clipboard** with the transcript --
  whatever you had copied is gone. The Save-Paste-Restore behavior that
  backs up and restores the clipboard lands in a later commit.
- Auto-paste uses `Ctrl+V`, which most **terminal emulators** ignore in
  favour of `Ctrl+Shift+V`. Terminal detection + bracketed-paste lands
  with Save-Paste-Restore.

## Explicitly deferred (coming in later commits)

- Save-Paste-Restore clipboard injection with configurable paste delay.
- Mouse Button 8/9 PTT (with `pynput` + `python-evdev` backends).
- Terminal detection and bracketed-paste fallback.
- WebSocket streaming (`/v1/stream`) for word-level incremental output.
- Client-side VAD (server-side VAD is already enabled).
- TLS, IP allowlists, Prometheus metrics, system tray icon.
- Rust client rewrite.

## Security note

The server is intended for **intranet** deployment. It binds `0.0.0.0`
by default and has no TLS of its own. If your network isn't trusted, at
minimum set `GHOSTSCRIBE_AUTH_TOKEN` and put a reverse proxy (nginx,
Caddy, Traefik) with TLS in front of it.

## License

MIT (see repo root when a `LICENSE` file is added).
