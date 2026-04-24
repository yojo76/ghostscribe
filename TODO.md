# TODO / Hardening Backlog

Items not tied to a specific plan file. Pick up when convenient.

## Duration / timeout hygiene

- [ ] **Client: cap recording duration.** Both Linux and Windows recorders accumulate audio chunks indefinitely while the trigger is held. Add a max-duration guard (e.g. 5 min default, configurable) that stops recording and either sends what's been captured or drops it with a warning. Prevents unbounded memory growth if the trigger gets stuck or a user holds it by accident.

- [ ] **Client: unify and expose HTTP timeout.** Linux hardcodes 30 s ([__main__.py:423](client/linux/ghostscribe_client/__main__.py#L423)), Windows hardcodes 60 s ([upload.rs:52](client/windows/src/upload.rs#L52)). Pick one default, make it configurable (`request_timeout_s` in config), document the relationship to recording duration × whisper speed.

- [ ] **Server: inference timeout.** `_do_transcribe` has no timeout around `engine.transcribe()`. A stuck inference holds the `asyncio.Semaphore(1)` and blocks every subsequent request. Wrap in `asyncio.wait_for()` with a configurable `GHOSTSCRIBE_INFERENCE_TIMEOUT_S` (default ~120 s) and return 504 on expiry.

## Mouse trigger

- [ ] **Windows Rust client: add mouse trigger support.** The Linux Rust client (Issue 2, in progress) will gain `mouse:x1`, `mouse:x2`, `mouse:back`, `mouse:forward` via rdev `ButtonPress`/`ButtonRelease`. The Windows client currently uses `WH_KEYBOARD_LL` only. Add a parallel `WH_MOUSE_LL` hook to handle `WM_XBUTTONDOWN`/`WM_XBUTTONUP` (side buttons X1/X2) and `WM_MBUTTONDOWN`/`WM_MBUTTONUP`. Reuse the `Trigger::Chord` / `Trigger::Mouse` enum and `mouse:` config prefix once the Linux design is finalised.

## Paste ergonomics

- [ ] **Client: smart-space continuation.** When the user dictates a second utterance into the same field, the paste collides with the previous text (`helloworld` instead of `hello world`). There's no reliable cross-app way to read the character before the cursor, so use a time-based heuristic instead: track `last_paste_monotonic` in the client; if the new trigger fires within `continuation_window_s` (default 30 s) and the transcript doesn't already start with whitespace, prepend a single space. Config fields: `smart_space = true`, `continuation_window_s = 30`. Applies to both Linux and Windows clients.
