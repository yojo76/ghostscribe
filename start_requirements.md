# Project: GhostScribe
## Vision: High-Speed, Private Intranet Speech-to-Text

GhostScribe is a low-latency, secure STT utility designed for intranet deployment. It allows users to dictate text directly into any focused application (Caret) using a Push-to-Talk (PTT) mechanic, offloading all heavy computation to a central GPU-accelerated server.

---

## 1. Core Architecture
- **Architecture:** Client-Server.
- **Server Hardware:** Dedicated intranet server with NVIDIA RTX 5060 Ti (16GB VRAM).
- **Inference Engine:** `Faster-Whisper` utilizing the `large-v3-turbo` model.
- **Model State:** Must remain resident in VRAM at all times for sub-second response.
- **Client Platforms:** Windows and Linux (specifically Linux Mint/X11).

## 2. Technical Stack
- **Server Framework:** Python / FastAPI.
- **Client Language:** Python (Preferred for MVP) or Rust (for performance/binary size).
- **Audio Specs:** 16kHz, Mono, 16-bit. Lossless compression (FLAC) or raw PCM (WAV).
- **Communication:** HTTP POST (PTT Release-to-Send)

## 3. Functional Requirements
### Trigger (PTT)
- Global hook for "Special" mouse buttons (typically Button 8/9 / Back/Forward). Or configured key configuration on the keyboard.
- System must record while the button is depressed and terminate/send on release.

### Text Injection (The "Save-Paste-Restore" Loop)
To avoid slow simulated typing and preserve user workflow:
1. **Backup:** Capture current system clipboard content.
2. **Payload:** Place transcribed text from server onto the clipboard.
3. **Execute:** Trigger `Ctrl+V` (or `Ctrl+Shift+V` for terminals).
4. **Wait:** Implement a configurable "Paste Buffer Delay" (e.g., 50ms-100ms) to ensure the target app processes the paste.
5. **Restore:** Revert clipboard to the original backed-up content.

### Compatibility
- Must detect if the active window is a Terminal/Shell.
- If Terminal, apply specific inter-character delays or "Bracketed Paste" logic if the clipboard method fails.

## 4. Performance & Security
- **Privacy:** 100% Intranet. No external API calls (OpenAI, Google, etc.).
- **Latency Target:** < 1.0s from "Button Release" to "Text Appears."
- **Concurrency:** Server must handle multiple concurrent requests via a processing queue.

---

## 5. Open Discussion Points (For Planning Mode)
*The following items require architectural analysis during the planning phase:*

1. **Input Simulation:** Should we use `pynput` for cross-platform hooks, or is `python-evdev` necessary for reliability on Linux Mint?
2. **The Linux "Primary" Selection:** On X11, should we utilize the `PRIMARY` selection (middle-click buffer) instead of the `CLIPBOARD` buffer to further protect user data?
3. **VAD (Voice Activity Detection):** Should the client run a lightweight VAD (like Silero) to trim silence before transmission to save bandwidth?
4. **Error Handling:** How should the client notify the user if the server is unreachable (e.g., a subtle system tray notification or an audible beep)?
5. **Context Injection:** Should the client send the Active Window Title to the server to allow the LLM/Whisper to adjust its "prompt" for technical vs. formal language?

---

## 6. Development Phases
- **Phase 1:** FastAPI server with `Faster-Whisper` + Simple cURL test.
- **Phase 2:** Python client with PTT recording and simple character-imitation typing.
- **Phase 3:** Implementation of the "Save-Paste-Restore" clipboard logic.
- **Phase 4:** Linux/Windows packaging and terminal-specific optimizations.