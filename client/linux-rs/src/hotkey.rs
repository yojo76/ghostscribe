//! Global push-to-talk trigger via rdev (X11 XRecord backend on Linux).
//!
//! ## Trigger syntax
//!
//! ```toml
//! trigger = "key:ctrl+g"            # single modifier
//! trigger = "key:ctrl+shift+g"      # multi-modifier chord
//! trigger = "key:super+space"       # Meta / Super key
//! trigger = "key:ctrl+alt+t"        # three modifiers
//! trigger = "mouse:x2"              # forward side button
//! trigger = "mouse:left"            # left mouse button
//! ```
//!
//! ### Modifier names
//! `ctrl`, `shift`, `alt`, `super` (Meta key), `meta` (alias for `super`).
//! Multiple modifiers are separated by `+`; the last `+`-segment is the key.
//!
//! ### Mouse button names
//! `left`, `right`, `middle`, `x1` (X11 button 8 / back), `x2` (button 9 / forward).
//!
//! ## Internals
//!
//! `rdev::listen` delivers both keyboard and mouse events in a single callback.
//! Modifiers are tracked as a bitmask (`MOD_*` constants). The chord fires when
//! all required modifier bits are set and the main key is also held.
//!
//! ## Prerequisites
//!
//! The X server must have XRecord enabled (standard on all desktop installs):
//! ```sh
//! xdpyinfo | grep XRecord
//! ```

use anyhow::{anyhow, Result};
use rdev::{listen, Button as RdevButton, Event, EventType, Key as RdevKey};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

// ── Modifier bitmask constants ────────────────────────────────────────────────

pub const MOD_CTRL:  u8 = 0b0001;
pub const MOD_SHIFT: u8 = 0b0010;
pub const MOD_ALT:   u8 = 0b0100;
/// Meta / Super key (`MetaLeft` / `MetaRight` in rdev).
pub const MOD_SUPER: u8 = 0b1000;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum HotkeyEvent {
    Press,
    Release,
    /// One-key recording interrupted by a foreign keystroke — discard buffer.
    Cancel,
}

/// Internal modifier enum — used only for `OneKeyTrigger` matching.
#[derive(Debug, Clone, Copy)]
enum Modifier { Ctrl, Alt }

/// Parsed keyboard chord. `modifiers` is a bitfield of `MOD_*` constants.
/// Zero means no modifier required (bare-key trigger).
#[derive(Debug, Clone, Copy)]
pub struct TriggerKeys {
    /// Bitmask of `MOD_*` constants. All set bits must be held simultaneously.
    pub modifiers: u8,
    /// Non-modifier key of the chord, mapped to an rdev::Key at parse time.
    pub key: RdevKey,
}

/// Mouse button used as a push-to-talk trigger.
#[derive(Debug, Clone, Copy)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// X11 button 8 / back.
    X1,
    /// X11 button 9 / forward.
    X2,
}

/// Parsed form of the `trigger` config string.
#[derive(Debug, Clone, Copy)]
pub enum TriggerConfig {
    Key(TriggerKeys),
    Mouse(MouseButton),
}

impl TriggerConfig {
    fn required_mods(self) -> u8 {
        match self {
            TriggerConfig::Key(tk) => tk.modifiers,
            TriggerConfig::Mouse(_) => 0,
        }
    }
}

/// Single-key push-to-talk. Restricted to modifiers and F-keys.
#[derive(Debug, Clone, Copy)]
pub enum OneKeyTrigger {
    Ctrl,
    Alt,
    Function(RdevKey),
}

// ── Parsing ───────────────────────────────────────────────────────────────────

pub fn parse_trigger(s: &str) -> Result<TriggerConfig> {
    let s = s.trim();

    if let Some(rest) = s.strip_prefix("mouse:") {
        let btn = parse_mouse_button(rest)
            .ok_or_else(|| anyhow!(
                "unknown mouse button {rest:?}; use left, right, middle, x1, x2"
            ))?;
        return Ok(TriggerConfig::Mouse(btn));
    }

    let rest = s
        .strip_prefix("key:")
        .ok_or_else(|| anyhow!("trigger must start with 'key:' or 'mouse:' (got {s:?})"))?;

    let parts: Vec<&str> = rest.split('+').collect();
    if parts.is_empty() || parts.last().map_or(true, |k| k.is_empty()) {
        return Err(anyhow!("trigger key name is missing (got {s:?})"));
    }
    let key_name  = *parts.last().unwrap();
    let mod_parts = &parts[..parts.len() - 1];

    let mut modifiers: u8 = 0;
    for mod_str in mod_parts {
        let bit = parse_modifier_bit(mod_str).ok_or_else(|| anyhow!(
            "unknown modifier {mod_str:?}; use ctrl, shift, alt, super (or meta)"
        ))?;
        modifiers |= bit;
    }

    let key = key_name_to_rdev(key_name)
        .ok_or_else(|| anyhow!("unknown key name {key_name:?}"))?;

    Ok(TriggerConfig::Key(TriggerKeys { modifiers, key }))
}

fn parse_modifier_bit(s: &str) -> Option<u8> {
    match s.to_lowercase().as_str() {
        "ctrl"           => Some(MOD_CTRL),
        "shift"          => Some(MOD_SHIFT),
        "alt"            => Some(MOD_ALT),
        "super" | "meta" => Some(MOD_SUPER),
        _ => None,
    }
}

fn parse_mouse_button(s: &str) -> Option<MouseButton> {
    match s.to_lowercase().as_str() {
        "left"   => Some(MouseButton::Left),
        "right"  => Some(MouseButton::Right),
        "middle" => Some(MouseButton::Middle),
        "x1"     => Some(MouseButton::X1),
        "x2"     => Some(MouseButton::X2),
        _        => None,
    }
}

pub fn parse_one_key_trigger(s: &str) -> Result<Option<OneKeyTrigger>> {
    let s = s.trim();
    if s.is_empty() { return Ok(None); }
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
                "one_key_trigger must be key:ctrl, key:alt, or key:f1..key:f12 (got {s:?})"
            ))
        }
    }
}

fn key_name_to_rdev(name: &str) -> Option<RdevKey> {
    let name = name.to_lowercase();
    if name.len() == 1 {
        let c = name.chars().next()?;
        if c.is_ascii_alphabetic() { return letter_key(c); }
        if c.is_ascii_digit()      { return digit_key(c); }
    }
    if let Some(n) = name.strip_prefix('f') {
        if let Ok(num) = n.parse::<u32>() { return function_key(num); }
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

// ── Runtime helpers ───────────────────────────────────────────────────────────

/// Return the `MOD_*` bitmask bit for an rdev Key, or 0 if not a tracked modifier.
fn key_to_mod_bit(key: &RdevKey) -> u8 {
    if matches!(key, RdevKey::ControlLeft | RdevKey::ControlRight) { return MOD_CTRL;  }
    if matches!(key, RdevKey::ShiftLeft   | RdevKey::ShiftRight)   { return MOD_SHIFT; }
    if matches!(key, RdevKey::Alt         | RdevKey::AltGr)        { return MOD_ALT;   }
    if matches!(key, RdevKey::MetaLeft    | RdevKey::MetaRight)    { return MOD_SUPER; }
    0
}

fn key_is_modifier(key: &RdevKey, modifier: Modifier) -> bool {
    match modifier {
        Modifier::Ctrl => matches!(key, RdevKey::ControlLeft | RdevKey::ControlRight),
        Modifier::Alt  => matches!(key, RdevKey::Alt | RdevKey::AltGr),
    }
}

fn key_matches_one_key(key: &RdevKey, trig: OneKeyTrigger) -> bool {
    match trig {
        OneKeyTrigger::Ctrl        => key_is_modifier(key, Modifier::Ctrl),
        OneKeyTrigger::Alt         => key_is_modifier(key, Modifier::Alt),
        OneKeyTrigger::Function(f) => key == &f,
    }
}

fn rdev_button_matches(btn: &RdevButton, mb: MouseButton) -> bool {
    match mb {
        MouseButton::Left   => matches!(btn, RdevButton::Left),
        MouseButton::Right  => matches!(btn, RdevButton::Right),
        MouseButton::Middle => matches!(btn, RdevButton::Middle),
        MouseButton::X1     => matches!(btn, RdevButton::Unknown(8)),
        MouseButton::X2     => matches!(btn, RdevButton::Unknown(9)),
    }
}

// ── Hook state machine ────────────────────────────────────────────────────────

const MODE_IDLE:    u8 = 0;
const MODE_CHORD:   u8 = 1;
const MODE_ONE_KEY: u8 = 2;
const MODE_LOCKOUT: u8 = 3;

struct HookState {
    tx: Sender<HotkeyEvent>,
    trigger: TriggerConfig,
    one_key: Option<OneKeyTrigger>,
    mode: AtomicU8,
    /// Bitmask of `MOD_*` bits currently held down.
    chord_mods_down: AtomicU8,
    chord_key_down:  AtomicBool,
}

static STATE: OnceLock<HookState> = OnceLock::new();

fn handle_event(event: Event) {
    let state = match STATE.get() {
        Some(s) => s,
        None => return,
    };

    // Mouse buttons: simple press/release, no lockout logic.
    match &event.event_type {
        EventType::ButtonPress(btn) => {
            if let TriggerConfig::Mouse(mb) = state.trigger {
                if rdev_button_matches(btn, mb)
                    && state.mode.load(Ordering::SeqCst) == MODE_IDLE
                {
                    state.mode.store(MODE_CHORD, Ordering::SeqCst);
                    let _ = state.tx.send(HotkeyEvent::Press);
                }
            }
            return;
        }
        EventType::ButtonRelease(btn) => {
            if let TriggerConfig::Mouse(mb) = state.trigger {
                if rdev_button_matches(btn, mb)
                    && state.mode.load(Ordering::SeqCst) == MODE_CHORD
                {
                    state.mode.store(MODE_IDLE, Ordering::SeqCst);
                    let _ = state.tx.send(HotkeyEvent::Release);
                }
            }
            return;
        }
        EventType::KeyPress(_) | EventType::KeyRelease(_) => {}
        _ => return,
    }

    let (key, is_down) = match event.event_type {
        EventType::KeyPress(k)   => (k, true),
        EventType::KeyRelease(k) => (k, false),
        _ => return,
    };

    let this_mod = key_to_mod_bit(&key);
    let is_chord_main = match state.trigger {
        TriggerConfig::Key(tk) => key == tk.key,
        TriggerConfig::Mouse(_) => false,
    };
    let is_one_key = state.one_key
        .map(|t| key_matches_one_key(&key, t))
        .unwrap_or(false);

    // Update running modifier bitmask.
    if is_down && this_mod != 0 {
        state.chord_mods_down.fetch_or(this_mod, Ordering::SeqCst);
    } else if !is_down && this_mod != 0 {
        state.chord_mods_down.fetch_and(!this_mod, Ordering::SeqCst);
    }
    if is_down  && is_chord_main { state.chord_key_down.store(true,  Ordering::SeqCst); }
    if !is_down && is_chord_main { state.chord_key_down.store(false, Ordering::SeqCst); }

    let mode      = state.mode.load(Ordering::SeqCst);
    let req_mods  = state.trigger.required_mods();
    let mods_down = state.chord_mods_down.load(Ordering::SeqCst);
    let key_down  = state.chord_key_down.load(Ordering::SeqCst);

    match (mode, is_down) {
        (MODE_IDLE, true) => {
            let chord_ok = match state.trigger {
                TriggerConfig::Key(_) =>
                    (mods_down & req_mods) == req_mods && key_down,
                TriggerConfig::Mouse(_) => false,
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
            if is_chord_main || (this_mod & req_mods) != 0 {
                state.mode.store(MODE_IDLE, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Release);
            }
        }
        (MODE_ONE_KEY, true) => {
            let neutral = is_one_key || is_chord_main || (this_mod & req_mods) != 0;
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

/// Install the global input listener and block the calling thread.
/// Must run on a dedicated thread — `rdev::listen` never returns normally.
pub fn run_hook(
    tx: Sender<HotkeyEvent>,
    trigger: TriggerConfig,
    one_key: Option<OneKeyTrigger>,
) -> Result<()> {
    let state = HookState {
        tx,
        trigger,
        one_key,
        mode:            AtomicU8::new(MODE_IDLE),
        chord_mods_down: AtomicU8::new(0),
        chord_key_down:  AtomicBool::new(false),
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

    fn key(r: Result<TriggerConfig>) -> TriggerKeys {
        match r.unwrap() {
            TriggerConfig::Key(tk) => tk,
            other => panic!("expected TriggerConfig::Key, got {other:?}"),
        }
    }

    // ── Single-modifier (backward compat) ────────────────────────────────────

    #[test]
    fn parse_ctrl_g() {
        let t = key(parse_trigger("key:ctrl+g"));
        assert_eq!(t.modifiers, MOD_CTRL);
        assert_eq!(t.key, RdevKey::KeyG);
    }

    #[test]
    fn parse_f11_no_modifier() {
        let t = key(parse_trigger("key:f11"));
        assert_eq!(t.modifiers, 0);
        assert_eq!(t.key, RdevKey::F11);
    }

    #[test]
    fn parse_bare_letter() {
        let t = key(parse_trigger("key:a"));
        assert_eq!(t.modifiers, 0);
        assert_eq!(t.key, RdevKey::KeyA);
    }

    // ── Multi-modifier chords ─────────────────────────────────────────────────

    #[test]
    fn parse_ctrl_shift_g() {
        let t = key(parse_trigger("key:ctrl+shift+g"));
        assert_eq!(t.modifiers, MOD_CTRL | MOD_SHIFT);
        assert_eq!(t.key, RdevKey::KeyG);
    }

    #[test]
    fn parse_ctrl_alt_t() {
        let t = key(parse_trigger("key:ctrl+alt+t"));
        assert_eq!(t.modifiers, MOD_CTRL | MOD_ALT);
        assert_eq!(t.key, RdevKey::KeyT);
    }

    #[test]
    fn parse_ctrl_shift_alt_f12() {
        let t = key(parse_trigger("key:ctrl+shift+alt+f12"));
        assert_eq!(t.modifiers, MOD_CTRL | MOD_SHIFT | MOD_ALT);
        assert_eq!(t.key, RdevKey::F12);
    }

    #[test]
    fn parse_duplicate_modifier_is_idempotent() {
        let t = key(parse_trigger("key:ctrl+ctrl+g"));
        assert_eq!(t.modifiers, MOD_CTRL);
    }

    // ── Super / Meta key ──────────────────────────────────────────────────────

    #[test]
    fn parse_super_space() {
        let t = key(parse_trigger("key:super+space"));
        assert_eq!(t.modifiers, MOD_SUPER);
        assert_eq!(t.key, RdevKey::Space);
    }

    #[test]
    fn parse_meta_alias() {
        let t = key(parse_trigger("key:meta+space"));
        assert_eq!(t.modifiers, MOD_SUPER);
    }

    #[test]
    fn parse_super_shift_g() {
        let t = key(parse_trigger("key:super+shift+g"));
        assert_eq!(t.modifiers, MOD_SUPER | MOD_SHIFT);
    }

    // ── Mouse triggers ────────────────────────────────────────────────────────

    #[test]
    fn parse_mouse_x2() {
        assert!(matches!(parse_trigger("mouse:x2").unwrap(), TriggerConfig::Mouse(MouseButton::X2)));
    }

    #[test]
    fn parse_mouse_x1() {
        assert!(matches!(parse_trigger("mouse:x1").unwrap(), TriggerConfig::Mouse(MouseButton::X1)));
    }

    #[test]
    fn parse_mouse_left() {
        assert!(matches!(parse_trigger("mouse:left").unwrap(), TriggerConfig::Mouse(MouseButton::Left)));
    }

    #[test]
    fn parse_mouse_unknown_errors() {
        assert!(parse_trigger("mouse:side").is_err());
    }

    // ── Error cases ───────────────────────────────────────────────────────────

    #[test]
    fn parse_missing_prefix_errors() {
        assert!(parse_trigger("ctrl+g").is_err());
    }

    #[test]
    fn parse_unknown_modifier_errors() {
        assert!(parse_trigger("key:hyper+g").unwrap_err()
            .to_string().contains("unknown modifier"));
    }

    #[test]
    fn parse_unknown_key_errors() {
        assert!(parse_trigger("key:ctrl+nonsense").unwrap_err()
            .to_string().contains("unknown key name"));
    }

    // ── one_key_trigger ───────────────────────────────────────────────────────

    #[test]
    fn one_key_empty_is_disabled() {
        assert!(parse_one_key_trigger("").unwrap().is_none());
    }

    #[test]
    fn one_key_ctrl_alt() {
        assert!(matches!(parse_one_key_trigger("key:ctrl").unwrap(), Some(OneKeyTrigger::Ctrl)));
        assert!(matches!(parse_one_key_trigger("key:alt").unwrap(),  Some(OneKeyTrigger::Alt)));
    }

    #[test]
    fn one_key_f1_to_f12() {
        for n in 1u32..=12 {
            let t = parse_one_key_trigger(&format!("key:f{n}")).unwrap().unwrap();
            assert!(matches!(t, OneKeyTrigger::Function(_)), "f{n}");
        }
    }

    #[test]
    fn one_key_rejects_chord() {
        assert!(parse_one_key_trigger("key:ctrl+g").is_err());
    }

    // ── Key maps ─────────────────────────────────────────────────────────────

    #[test]
    fn letter_keys_cover_a_to_z() {
        for c in 'a'..='z' {
            assert!(letter_key(c).is_some(), "missing {c}");
        }
    }

    #[test]
    fn function_keys_cover_f1_to_f12() {
        for n in 1u32..=12 {
            assert!(function_key(n).is_some(), "f{n}");
        }
        assert!(function_key(0).is_none());
        assert!(function_key(13).is_none());
    }

    // ── key_to_mod_bit ────────────────────────────────────────────────────────

    #[test]
    fn mod_bit_recognises_all_modifiers() {
        assert_eq!(key_to_mod_bit(&RdevKey::ControlLeft),  MOD_CTRL);
        assert_eq!(key_to_mod_bit(&RdevKey::ControlRight), MOD_CTRL);
        assert_eq!(key_to_mod_bit(&RdevKey::ShiftLeft),    MOD_SHIFT);
        assert_eq!(key_to_mod_bit(&RdevKey::ShiftRight),   MOD_SHIFT);
        assert_eq!(key_to_mod_bit(&RdevKey::Alt),          MOD_ALT);
        assert_eq!(key_to_mod_bit(&RdevKey::AltGr),        MOD_ALT);
        assert_eq!(key_to_mod_bit(&RdevKey::MetaLeft),     MOD_SUPER);
        assert_eq!(key_to_mod_bit(&RdevKey::MetaRight),    MOD_SUPER);
        assert_eq!(key_to_mod_bit(&RdevKey::KeyG),         0);
    }
}
