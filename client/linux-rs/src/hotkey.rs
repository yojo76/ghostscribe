//! Global push-to-talk hotkey via rdev (X11 XRecord backend on Linux).
//!
//! The public API — `HotkeyEvent`, `TriggerKeys`, `Modifier`,
//! `OneKeyTrigger`, `parse_trigger`, `parse_one_key_trigger`, `run_hook` —
//! is intentionally identical to the Windows client's hotkey module so that
//! `main.rs` requires minimal changes.
//!
//! Key names in the config strings use the same syntax as Windows
//! (`key:ctrl+g`, `key:f11`, …) even though internally we map them to
//! `rdev::Key` variants rather than Win32 virtual-key codes.
//!
//! ## Prerequisites
//!
//! `rdev` on Linux uses the X11 XRecord extension to receive global key
//! events without root access. The X server must have XRecord enabled
//! (it is on every standard desktop install). If `run_hook` returns an
//! error, verify that `DISPLAY` is set and that XRecord is available:
//!
//! ```sh
//! xdpyinfo | grep XRecord
//! ```

use anyhow::{anyhow, Result};
use rdev::{listen, Event, EventType, Key as RdevKey};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum HotkeyEvent {
    Press,
    Release,
    /// One-key recording was interrupted by a foreign keystroke — discard
    /// the buffer without sending. Only emitted from `MODE_ONE_KEY`.
    Cancel,
}

#[derive(Debug, Clone, Copy)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
}

#[derive(Debug, Clone)]
pub struct TriggerKeys {
    pub modifier: Option<Modifier>,
    /// The non-modifier key of the chord, mapped to an rdev::Key at parse
    /// time so matching is a simple equality check at runtime.
    pub key: RdevKey,
}

/// Single-key push-to-talk trigger. Intentionally restricted to keys that
/// don't produce text on their own (modifiers or F-keys) to avoid hijacking
/// normal typing.
#[derive(Debug, Clone, Copy)]
pub enum OneKeyTrigger {
    Ctrl,
    Alt,
    Function(RdevKey),
}

// ── Parsing ───────────────────────────────────────────────────────────────────

pub fn parse_trigger(s: &str) -> Result<TriggerKeys> {
    let s = s.trim();
    let rest = s
        .strip_prefix("key:")
        .ok_or_else(|| anyhow!("trigger must start with 'key:' (got {s:?})"))?;

    let (modifier, key_name) = if let Some(plus) = rest.find('+') {
        let mod_str = &rest[..plus];
        let key_str = &rest[plus + 1..];
        let modifier = match mod_str {
            "ctrl"  => Modifier::Ctrl,
            "shift" => Modifier::Shift,
            "alt"   => Modifier::Alt,
            other => return Err(anyhow!("unknown modifier {other:?}; use ctrl, shift, or alt")),
        };
        (Some(modifier), key_str)
    } else {
        (None, rest)
    };

    let key = key_name_to_rdev(key_name)
        .ok_or_else(|| anyhow!("unknown key name {key_name:?}"))?;

    Ok(TriggerKeys { modifier, key })
}

pub fn parse_one_key_trigger(s: &str) -> Result<Option<OneKeyTrigger>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let rest = if s.len() >= 4 && s[..4].eq_ignore_ascii_case("key:") {
        &s[4..]
    } else {
        return Err(anyhow!("one_key_trigger must start with 'key:' (got {s:?})"));
    };
    if rest.contains('+') {
        return Err(anyhow!(
            "one_key_trigger cannot be a chord (got {s:?}); use trigger= for chords"
        ));
    }
    let lower = rest.to_lowercase();
    match lower.as_str() {
        "ctrl" => Ok(Some(OneKeyTrigger::Ctrl)),
        "alt"  => Ok(Some(OneKeyTrigger::Alt)),
        _ => {
            if let Some(n) = lower.strip_prefix('f') {
                if let Ok(num) = n.parse::<u32>() {
                    if let Some(key) = function_key(num) {
                        return Ok(Some(OneKeyTrigger::Function(key)));
                    }
                }
            }
            Err(anyhow!(
                "one_key_trigger must be one of: key:ctrl, key:alt, key:f1..key:f12 (got {s:?})"
            ))
        }
    }
}

fn key_name_to_rdev(name: &str) -> Option<RdevKey> {
    let name = name.to_lowercase();
    if name.len() == 1 {
        let c = name.chars().next()?;
        if c.is_ascii_alphabetic() {
            return letter_key(c);
        }
        if c.is_ascii_digit() {
            return digit_key(c);
        }
    }
    if let Some(n) = name.strip_prefix('f') {
        if let Ok(num) = n.parse::<u32>() {
            return function_key(num);
        }
    }
    match name.as_str() {
        "space"       => Some(RdevKey::Space),
        "escape"      => Some(RdevKey::Escape),
        "return"      => Some(RdevKey::Return),
        "tab"         => Some(RdevKey::Tab),
        "backspace"   => Some(RdevKey::Backspace),
        "delete"      => Some(RdevKey::Delete),
        "insert"      => Some(RdevKey::Insert),
        "home"        => Some(RdevKey::Home),
        "end"         => Some(RdevKey::End),
        "pageup"      => Some(RdevKey::PageUp),
        "pagedown"    => Some(RdevKey::PageDown),
        "left"        => Some(RdevKey::LeftArrow),
        "up"          => Some(RdevKey::UpArrow),
        "right"       => Some(RdevKey::RightArrow),
        "down"        => Some(RdevKey::DownArrow),
        "capslock"    => Some(RdevKey::CapsLock),
        "numlock"     => Some(RdevKey::NumLock),
        "scrolllock"  => Some(RdevKey::ScrollLock),
        "pause"       => Some(RdevKey::Pause),
        "printscreen" => Some(RdevKey::PrintScreen),
        _             => None,
    }
}

fn letter_key(c: char) -> Option<RdevKey> {
    match c {
        'a' => Some(RdevKey::KeyA), 'b' => Some(RdevKey::KeyB),
        'c' => Some(RdevKey::KeyC), 'd' => Some(RdevKey::KeyD),
        'e' => Some(RdevKey::KeyE), 'f' => Some(RdevKey::KeyF),
        'g' => Some(RdevKey::KeyG), 'h' => Some(RdevKey::KeyH),
        'i' => Some(RdevKey::KeyI), 'j' => Some(RdevKey::KeyJ),
        'k' => Some(RdevKey::KeyK), 'l' => Some(RdevKey::KeyL),
        'm' => Some(RdevKey::KeyM), 'n' => Some(RdevKey::KeyN),
        'o' => Some(RdevKey::KeyO), 'p' => Some(RdevKey::KeyP),
        'q' => Some(RdevKey::KeyQ), 'r' => Some(RdevKey::KeyR),
        's' => Some(RdevKey::KeyS), 't' => Some(RdevKey::KeyT),
        'u' => Some(RdevKey::KeyU), 'v' => Some(RdevKey::KeyV),
        'w' => Some(RdevKey::KeyW), 'x' => Some(RdevKey::KeyX),
        'y' => Some(RdevKey::KeyY), 'z' => Some(RdevKey::KeyZ),
        _   => None,
    }
}

fn digit_key(c: char) -> Option<RdevKey> {
    match c {
        '0' => Some(RdevKey::Num0), '1' => Some(RdevKey::Num1),
        '2' => Some(RdevKey::Num2), '3' => Some(RdevKey::Num3),
        '4' => Some(RdevKey::Num4), '5' => Some(RdevKey::Num5),
        '6' => Some(RdevKey::Num6), '7' => Some(RdevKey::Num7),
        '8' => Some(RdevKey::Num8), '9' => Some(RdevKey::Num9),
        _   => None,
    }
}

fn function_key(n: u32) -> Option<RdevKey> {
    match n {
        1  => Some(RdevKey::F1),  2  => Some(RdevKey::F2),
        3  => Some(RdevKey::F3),  4  => Some(RdevKey::F4),
        5  => Some(RdevKey::F5),  6  => Some(RdevKey::F6),
        7  => Some(RdevKey::F7),  8  => Some(RdevKey::F8),
        9  => Some(RdevKey::F9),  10 => Some(RdevKey::F10),
        11 => Some(RdevKey::F11), 12 => Some(RdevKey::F12),
        _  => None, // F13-F24: rdev 0.5 maps these to Unknown(keysym); TODO
    }
}

// ── Key matching helpers ──────────────────────────────────────────────────────

fn key_is_modifier(key: &RdevKey, modifier: Modifier) -> bool {
    match modifier {
        Modifier::Ctrl  => matches!(key, RdevKey::ControlLeft | RdevKey::ControlRight),
        Modifier::Shift => matches!(key, RdevKey::ShiftLeft | RdevKey::ShiftRight),
        Modifier::Alt   => matches!(key, RdevKey::Alt | RdevKey::AltGr),
    }
}

fn key_matches_one_key(key: &RdevKey, trig: OneKeyTrigger) -> bool {
    match trig {
        OneKeyTrigger::Ctrl        => key_is_modifier(key, Modifier::Ctrl),
        OneKeyTrigger::Alt         => key_is_modifier(key, Modifier::Alt),
        OneKeyTrigger::Function(f) => key == &f,
    }
}

// ── Hook state machine ────────────────────────────────────────────────────────

const MODE_IDLE:    u8 = 0;
const MODE_CHORD:   u8 = 1;
const MODE_ONE_KEY: u8 = 2;
const MODE_LOCKOUT: u8 = 3;

struct HookState {
    tx: Sender<HotkeyEvent>,
    trigger: TriggerKeys,
    one_key: Option<OneKeyTrigger>,
    mode: AtomicU8,
    chord_mod_down: AtomicBool,
    chord_key_down: AtomicBool,
}

static STATE: OnceLock<HookState> = OnceLock::new();

fn handle_event(event: Event) {
    let state = match STATE.get() {
        Some(s) => s,
        None => return,
    };

    let (key, is_down) = match event.event_type {
        EventType::KeyPress(k)   => (k, true),
        EventType::KeyRelease(k) => (k, false),
        _ => return,
    };

    let is_chord_mod  = state.trigger.modifier
        .map(|m| key_is_modifier(&key, m))
        .unwrap_or(false);
    let is_chord_main = key == state.trigger.key;
    let is_one_key    = state.one_key
        .map(|t| key_matches_one_key(&key, t))
        .unwrap_or(false);

    if is_down {
        if is_chord_mod  { state.chord_mod_down.store(true,  Ordering::SeqCst); }
        if is_chord_main { state.chord_key_down.store(true,  Ordering::SeqCst); }
    } else {
        if is_chord_mod  { state.chord_mod_down.store(false, Ordering::SeqCst); }
        if is_chord_main { state.chord_key_down.store(false, Ordering::SeqCst); }
    }

    let mode = state.mode.load(Ordering::SeqCst);

    match (mode, is_down) {
        (MODE_IDLE, true) => {
            let chord_ok = match state.trigger.modifier {
                Some(_) => state.chord_mod_down.load(Ordering::SeqCst)
                        && state.chord_key_down.load(Ordering::SeqCst),
                None    => state.chord_key_down.load(Ordering::SeqCst),
            };
            if chord_ok {
                state.mode.store(MODE_CHORD, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Press);
            } else if is_one_key {
                state.mode.store(MODE_ONE_KEY, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Press);
            }
        }
        (MODE_CHORD, false) => {
            if is_chord_main || is_chord_mod {
                state.mode.store(MODE_IDLE, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Release);
            }
        }
        (MODE_ONE_KEY, true) => {
            let neutral = is_one_key || is_chord_main || is_chord_mod;
            if !neutral {
                state.mode.store(MODE_LOCKOUT, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Cancel);
            }
        }
        (MODE_ONE_KEY, false) => {
            if is_one_key {
                state.mode.store(MODE_IDLE, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Release);
            }
        }
        (MODE_LOCKOUT, false) => {
            if is_one_key {
                state.mode.store(MODE_IDLE, Ordering::SeqCst);
            }
        }
        _ => {}
    }
}

/// Install the global key listener and block the calling thread.
/// Must run on a dedicated thread — `rdev::listen` never returns normally.
pub fn run_hook(
    tx: Sender<HotkeyEvent>,
    trigger: TriggerKeys,
    one_key: Option<OneKeyTrigger>,
) -> Result<()> {
    let state = HookState {
        tx,
        trigger,
        one_key,
        mode: AtomicU8::new(MODE_IDLE),
        chord_mod_down: AtomicBool::new(false),
        chord_key_down: AtomicBool::new(false),
    };
    STATE
        .set(state)
        .map_err(|_| anyhow!("hotkey hook already initialised"))?;

    listen(handle_event).map_err(|e| anyhow!("rdev::listen failed: {e:?}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ctrl_g() {
        let t = parse_trigger("key:ctrl+g").unwrap();
        assert!(matches!(t.modifier, Some(Modifier::Ctrl)));
        assert_eq!(t.key, RdevKey::KeyG);
    }

    #[test]
    fn parse_f11_no_modifier() {
        let t = parse_trigger("key:f11").unwrap();
        assert!(t.modifier.is_none());
        assert_eq!(t.key, RdevKey::F11);
    }

    #[test]
    fn parse_bare_letter() {
        let t = parse_trigger("key:a").unwrap();
        assert!(t.modifier.is_none());
        assert_eq!(t.key, RdevKey::KeyA);
    }

    #[test]
    fn parse_missing_prefix_errors() {
        assert!(parse_trigger("ctrl+g").is_err());
    }

    #[test]
    fn parse_unknown_modifier_errors() {
        assert!(parse_trigger("key:hyper+g").is_err());
    }

    #[test]
    fn parse_one_key_empty_is_disabled() {
        assert!(parse_one_key_trigger("").unwrap().is_none());
    }

    #[test]
    fn parse_one_key_ctrl_alt() {
        assert!(matches!(parse_one_key_trigger("key:ctrl").unwrap(), Some(OneKeyTrigger::Ctrl)));
        assert!(matches!(parse_one_key_trigger("key:alt").unwrap(),  Some(OneKeyTrigger::Alt)));
    }

    #[test]
    fn parse_one_key_f1_to_f12() {
        for n in 1u32..=12 {
            let s = format!("key:f{n}");
            let t = parse_one_key_trigger(&s).unwrap().unwrap();
            assert!(matches!(t, OneKeyTrigger::Function(_)), "f{n}");
        }
    }

    #[test]
    fn parse_one_key_rejects_chord() {
        assert!(parse_one_key_trigger("key:ctrl+g").is_err());
    }

    #[test]
    fn letter_keys_cover_a_to_z() {
        for c in 'a'..='z' {
            assert!(letter_key(c).is_some(), "missing letter {c}");
        }
    }

    #[test]
    fn digit_keys_cover_0_to_9() {
        for c in '0'..='9' {
            assert!(digit_key(c).is_some(), "missing digit {c}");
        }
    }

    #[test]
    fn function_keys_cover_f1_to_f12() {
        for n in 1u32..=12 {
            assert!(function_key(n).is_some(), "missing f{n}");
        }
        assert!(function_key(0).is_none());
        assert!(function_key(13).is_none());
    }
}
