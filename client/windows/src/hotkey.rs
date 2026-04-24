//! Global push-to-talk trigger: keyboard chord / one-key, or mouse button.
//!
//! ## Trigger syntax
//!
//! ```toml
//! trigger = "key:ctrl+g"            # single modifier (original)
//! trigger = "key:ctrl+shift+g"      # multi-modifier chord
//! trigger = "key:super+space"       # Windows/Super key
//! trigger = "key:ctrl+alt+del"      # three-modifier chord
//! trigger = "mouse:x2"              # forward side button
//! trigger = "mouse:left"            # left mouse button
//! ```
//!
//! ### Modifier names
//! `ctrl`, `shift`, `alt`, `super` (Win key), `win` (alias for `super`).
//! Multiple modifiers are separated by `+`; the last `+`-segment is the key.
//!
//! ### Mouse button names
//! `left`, `right`, `middle`, `x1` (back), `x2` (forward).
//!
//! ## Internals
//!
//! Modifiers are tracked as a bitmask (`MOD_*` constants). The chord fires
//! when every required modifier bit is set and the main key is also held.
//! A `WH_KEYBOARD_LL` hook is always installed; a `WH_MOUSE_LL` hook is
//! additionally installed when `trigger` is `TriggerConfig::Mouse`.

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_LCONTROL, VK_RCONTROL, VK_LSHIFT, VK_RSHIFT, VK_LMENU, VK_RMENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT, MSG,
    WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

// Windows key VK codes (not in the feature-gated re-export we use).
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

// ── Modifier bitmask constants ────────────────────────────────────────────────

pub const MOD_CTRL:  u8 = 0b0001;
pub const MOD_SHIFT: u8 = 0b0010;
pub const MOD_ALT:   u8 = 0b0100;
/// Windows/Super key (`VK_LWIN` / `VK_RWIN`).
pub const MOD_SUPER: u8 = 0b1000;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum HotkeyEvent {
    Press,
    Release,
    /// One-key recording was interrupted by a foreign keystroke — discard
    /// the buffer without sending. Only emitted from `MODE_ONE_KEY`.
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
    pub key: u32,
}

/// Mouse button used as a push-to-talk trigger.
#[derive(Debug, Clone, Copy)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// Side / back button (XButton1).
    X1,
    /// Side / forward button (XButton2).
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

/// Single-key push-to-talk. Restricted to modifiers and F-keys so it cannot
/// hijack normal typing.
#[derive(Debug, Clone, Copy)]
pub enum OneKeyTrigger {
    Ctrl,
    Alt,
    /// Raw VK code (e.g. 0x70 for F1).
    Function(u32),
}

// ── Parsing ───────────────────────────────────────────────────────────────────

pub fn parse_trigger(s: &str) -> Result<TriggerConfig> {
    let s = s.trim();

    // Mouse trigger: mouse:<button>
    if let Some(rest) = s.strip_prefix("mouse:") {
        let btn = parse_mouse_button(rest)
            .ok_or_else(|| anyhow!(
                "unknown mouse button {rest:?}; use left, right, middle, x1, x2"
            ))?;
        return Ok(TriggerConfig::Mouse(btn));
    }

    // Keyboard trigger: key:[mod+…+]<keyname>
    let rest = s
        .strip_prefix("key:")
        .ok_or_else(|| anyhow!("trigger must start with 'key:' or 'mouse:' (got {s:?})"))?;

    // Split on '+'. Everything except the last segment is a modifier name.
    let parts: Vec<&str> = rest.split('+').collect();
    if parts.is_empty() || parts.last().map_or(true, |k| k.is_empty()) {
        return Err(anyhow!("trigger key name is missing (got {s:?})"));
    }
    let key_name   = *parts.last().unwrap();
    let mod_parts  = &parts[..parts.len() - 1];

    let mut modifiers: u8 = 0;
    for mod_str in mod_parts {
        let bit = parse_modifier_bit(mod_str).ok_or_else(|| anyhow!(
            "unknown modifier {mod_str:?}; use ctrl, shift, alt, super (or win)"
        ))?;
        modifiers |= bit;
    }

    let key = key_name_to_vk(key_name)
        .ok_or_else(|| anyhow!("unknown key name {key_name:?}"))?;

    Ok(TriggerConfig::Key(TriggerKeys { modifiers, key }))
}

fn parse_modifier_bit(s: &str) -> Option<u8> {
    match s.to_lowercase().as_str() {
        "ctrl"  => Some(MOD_CTRL),
        "shift" => Some(MOD_SHIFT),
        "alt"   => Some(MOD_ALT),
        "super" | "win" => Some(MOD_SUPER),
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
                    if (1..=24).contains(&num) {
                        return Ok(Some(OneKeyTrigger::Function(0x6F + num)));
                    }
                }
            }
            Err(anyhow!(
                "one_key_trigger must be key:ctrl, key:alt, or key:f1..key:f24 (got {s:?})"
            ))
        }
    }
}

fn key_name_to_vk(name: &str) -> Option<u32> {
    let name = name.to_lowercase();
    if name.len() == 1 {
        let c = name.chars().next()?;
        if c.is_ascii_alphabetic() { return Some(c.to_ascii_uppercase() as u32); }
        if c.is_ascii_digit()      { return Some(c as u32); }
    }
    if let Some(n) = name.strip_prefix('f') {
        if let Ok(num) = n.parse::<u32>() {
            if (1..=24).contains(&num) { return Some(0x6F + num); }
        }
    }
    match name.as_str() {
        "pause"       => Some(0x13),
        "capslock"    => Some(0x14),
        "escape"      => Some(0x1B),
        "space"       => Some(0x20),
        "insert"      => Some(0x2D),
        "delete"      => Some(0x2E),
        "home"        => Some(0x24),
        "end"         => Some(0x23),
        "pageup"      => Some(0x21),
        "pagedown"    => Some(0x22),
        "left"        => Some(0x25),
        "up"          => Some(0x26),
        "right"       => Some(0x27),
        "down"        => Some(0x28),
        "scroll"      => Some(0x91),
        "numlock"     => Some(0x90),
        "printscreen" => Some(0x2C),
        _ => None,
    }
}

// ── Runtime helpers ───────────────────────────────────────────────────────────

/// Return the `MOD_*` bitmask bit for a VK code, or 0 if not a tracked modifier.
fn vk_to_mod_bit(vk: u32) -> u8 {
    if vk == VK_LCONTROL.0 as u32 || vk == VK_RCONTROL.0 as u32 { return MOD_CTRL;  }
    if vk == VK_LSHIFT.0 as u32   || vk == VK_RSHIFT.0 as u32   { return MOD_SHIFT; }
    if vk == VK_LMENU.0 as u32    || vk == VK_RMENU.0 as u32    { return MOD_ALT;   }
    if vk == VK_LWIN               || vk == VK_RWIN               { return MOD_SUPER; }
    0
}

fn is_modifier_vk(vk: u32, modifier: Modifier) -> bool {
    match modifier {
        Modifier::Ctrl => vk == VK_LCONTROL.0 as u32 || vk == VK_RCONTROL.0 as u32,
        Modifier::Alt  => vk == VK_LMENU.0 as u32    || vk == VK_RMENU.0 as u32,
    }
}

fn matches_one_key(vk: u32, trig: OneKeyTrigger) -> bool {
    match trig {
        OneKeyTrigger::Ctrl        => is_modifier_vk(vk, Modifier::Ctrl),
        OneKeyTrigger::Alt         => is_modifier_vk(vk, Modifier::Alt),
        OneKeyTrigger::Function(f) => vk == f,
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

// ── Keyboard hook ─────────────────────────────────────────────────────────────

unsafe extern "system" fn keyboard_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code >= 0 {
        let kb  = &*(l_param.0 as *const KBDLLHOOKSTRUCT);
        let vk  = kb.vkCode;
        let msg = w_param.0 as u32;

        if let Some(state) = STATE.get() {
            let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let is_up   = msg == WM_KEYUP   || msg == WM_SYSKEYUP;

            let this_mod = vk_to_mod_bit(vk);
            let is_chord_main = match state.trigger {
                TriggerConfig::Key(tk) => vk == tk.key,
                TriggerConfig::Mouse(_) => false,
            };
            let is_one_key = state.one_key
                .map(|t| matches_one_key(vk, t))
                .unwrap_or(false);

            // Update running modifier bitmask.
            if is_down && this_mod != 0 {
                state.chord_mods_down.fetch_or(this_mod, Ordering::SeqCst);
            } else if is_up && this_mod != 0 {
                state.chord_mods_down.fetch_and(!this_mod, Ordering::SeqCst);
            }
            if is_down && is_chord_main { state.chord_key_down.store(true,  Ordering::SeqCst); }
            if is_up   && is_chord_main { state.chord_key_down.store(false, Ordering::SeqCst); }

            let mode        = state.mode.load(Ordering::SeqCst);
            let req_mods    = state.trigger.required_mods();
            let mods_down   = state.chord_mods_down.load(Ordering::SeqCst);
            let key_down    = state.chord_key_down.load(Ordering::SeqCst);

            match (mode, is_down, is_up) {
                (MODE_IDLE, true, _) => {
                    // Chord fires when all required modifiers AND the main key are held.
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
                (MODE_CHORD, _, true) => {
                    // Release on main key or any required modifier.
                    if is_chord_main || (this_mod & req_mods) != 0 {
                        state.mode.store(MODE_IDLE, Ordering::SeqCst);
                        let _ = state.tx.send(HotkeyEvent::Release);
                    }
                }
                (MODE_ONE_KEY, true, _) => {
                    // Keys neutral in one-key mode: the one-key itself, or any key
                    // that is part of the chord trigger (main key + any required modifier).
                    let neutral = is_one_key || is_chord_main || (this_mod & req_mods) != 0;
                    if !neutral {
                        state.mode.store(MODE_LOCKOUT, Ordering::SeqCst);
                        let _ = state.tx.send(HotkeyEvent::Cancel);
                    }
                }
                (MODE_ONE_KEY, _, true) => {
                    if is_one_key {
                        state.mode.store(MODE_IDLE, Ordering::SeqCst);
                        let _ = state.tx.send(HotkeyEvent::Release);
                    }
                }
                (MODE_LOCKOUT, _, true) => {
                    if is_one_key {
                        state.mode.store(MODE_IDLE, Ordering::SeqCst);
                    }
                }
                _ => {}
            }
        }
    }
    CallNextHookEx(HHOOK::default(), n_code, w_param, l_param)
}

// ── Mouse hook ────────────────────────────────────────────────────────────────

fn mouse_btn_down(msg: u32, btn: MouseButton, mouse_data: u32) -> bool {
    match btn {
        MouseButton::Left   => msg == WM_LBUTTONDOWN,
        MouseButton::Right  => msg == WM_RBUTTONDOWN,
        MouseButton::Middle => msg == WM_MBUTTONDOWN,
        MouseButton::X1     => msg == WM_XBUTTONDOWN && (mouse_data >> 16) as u16 == 1,
        MouseButton::X2     => msg == WM_XBUTTONDOWN && (mouse_data >> 16) as u16 == 2,
    }
}

fn mouse_btn_up(msg: u32, btn: MouseButton, mouse_data: u32) -> bool {
    match btn {
        MouseButton::Left   => msg == WM_LBUTTONUP,
        MouseButton::Right  => msg == WM_RBUTTONUP,
        MouseButton::Middle => msg == WM_MBUTTONUP,
        MouseButton::X1     => msg == WM_XBUTTONUP && (mouse_data >> 16) as u16 == 1,
        MouseButton::X2     => msg == WM_XBUTTONUP && (mouse_data >> 16) as u16 == 2,
    }
}

unsafe extern "system" fn mouse_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code >= 0 {
        if let Some(state) = STATE.get() {
            if let TriggerConfig::Mouse(btn) = state.trigger {
                let ms   = &*(l_param.0 as *const MSLLHOOKSTRUCT);
                let msg  = w_param.0 as u32;
                let mode = state.mode.load(Ordering::SeqCst);
                if mouse_btn_down(msg, btn, ms.mouseData) && mode == MODE_IDLE {
                    state.mode.store(MODE_CHORD, Ordering::SeqCst);
                    let _ = state.tx.send(HotkeyEvent::Press);
                } else if mouse_btn_up(msg, btn, ms.mouseData) && mode == MODE_CHORD {
                    state.mode.store(MODE_IDLE, Ordering::SeqCst);
                    let _ = state.tx.send(HotkeyEvent::Release);
                }
            }
        }
    }
    CallNextHookEx(HHOOK::default(), n_code, w_param, l_param)
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Installs low-level input hooks and runs the Windows message pump.
/// Blocks until `WM_QUIT`. Must run on a dedicated thread.
pub fn run_hook(
    tx: Sender<HotkeyEvent>,
    trigger: TriggerConfig,
    one_key: Option<OneKeyTrigger>,
) -> Result<()> {
    let state = HookState {
        tx,
        trigger,
        one_key,
        mode:             AtomicU8::new(MODE_IDLE),
        chord_mods_down:  AtomicU8::new(0),
        chord_key_down:   AtomicBool::new(false),
    };
    STATE
        .set(state)
        .map_err(|_| anyhow!("hotkey hook already initialised"))?;

    unsafe {
        let kb_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0)
            .map_err(|e| anyhow!("SetWindowsHookExW (keyboard) failed: {e}"))?;

        let mouse_hook = if matches!(STATE.get().unwrap().trigger, TriggerConfig::Mouse(_)) {
            Some(
                SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0)
                    .map_err(|e| anyhow!("SetWindowsHookExW (mouse) failed: {e}"))?,
            )
        } else {
            None
        };

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = UnhookWindowsHookEx(kb_hook);
        if let Some(h) = mouse_hook { let _ = UnhookWindowsHookEx(h); }
    }
    Ok(())
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
        assert_eq!(t.key, b'G' as u32);
    }

    #[test]
    fn parse_f12_no_modifier() {
        let t = key(parse_trigger("key:f12"));
        assert_eq!(t.modifiers, 0);
        assert_eq!(t.key, 0x7B);
    }

    #[test]
    fn parse_bare_letter() {
        let t = key(parse_trigger("key:a"));
        assert_eq!(t.modifiers, 0);
        assert_eq!(t.key, b'A' as u32);
    }

    #[test]
    fn parse_shift_f5() {
        let t = key(parse_trigger("key:shift+f5"));
        assert_eq!(t.modifiers, MOD_SHIFT);
        assert_eq!(t.key, 0x6F + 5); // VK_F5
    }

    // ── Multi-modifier chords ─────────────────────────────────────────────────

    #[test]
    fn parse_ctrl_shift_g() {
        let t = key(parse_trigger("key:ctrl+shift+g"));
        assert_eq!(t.modifiers, MOD_CTRL | MOD_SHIFT);
        assert_eq!(t.key, b'G' as u32);
    }

    #[test]
    fn parse_ctrl_alt_delete() {
        let t = key(parse_trigger("key:ctrl+alt+delete"));
        assert_eq!(t.modifiers, MOD_CTRL | MOD_ALT);
        assert_eq!(t.key, 0x2E); // VK_DELETE
    }

    #[test]
    fn parse_ctrl_shift_alt_f12() {
        let t = key(parse_trigger("key:ctrl+shift+alt+f12"));
        assert_eq!(t.modifiers, MOD_CTRL | MOD_SHIFT | MOD_ALT);
        assert_eq!(t.key, 0x7B); // VK_F12
    }

    #[test]
    fn parse_duplicate_modifier_is_idempotent() {
        let t = key(parse_trigger("key:ctrl+ctrl+g"));
        assert_eq!(t.modifiers, MOD_CTRL);
        assert_eq!(t.key, b'G' as u32);
    }

    // ── Super / Win key ───────────────────────────────────────────────────────

    #[test]
    fn parse_super_space() {
        let t = key(parse_trigger("key:super+space"));
        assert_eq!(t.modifiers, MOD_SUPER);
        assert_eq!(t.key, 0x20); // VK_SPACE
    }

    #[test]
    fn parse_win_alias() {
        let t = key(parse_trigger("key:win+space"));
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
    fn parse_mouse_buttons() {
        use MouseButton::*;
        let cases = [("left", Left), ("right", Right), ("middle", Middle)];
        for (s, expected) in cases {
            let r = parse_trigger(&format!("mouse:{s}")).unwrap();
            assert!(matches!((r, expected),
                (TriggerConfig::Mouse(Left),   Left)   |
                (TriggerConfig::Mouse(Right),  Right)  |
                (TriggerConfig::Mouse(Middle), Middle)
            ), "{s}");
        }
    }

    #[test]
    fn parse_mouse_case_insensitive() {
        assert!(matches!(parse_trigger("mouse:X2").unwrap(), TriggerConfig::Mouse(MouseButton::X2)));
    }

    #[test]
    fn parse_mouse_unknown_errors() {
        assert!(parse_trigger("mouse:side").unwrap_err().to_string().contains("unknown mouse button"));
    }

    // ── Error cases ───────────────────────────────────────────────────────────

    #[test]
    fn parse_missing_prefix_errors() {
        let err = parse_trigger("ctrl+g").unwrap_err();
        assert!(err.to_string().contains("must start with 'key:'"));
    }

    #[test]
    fn parse_unknown_modifier_errors() {
        let err = parse_trigger("key:hyper+g").unwrap_err();
        assert!(err.to_string().contains("unknown modifier"));
    }

    #[test]
    fn parse_unknown_key_errors() {
        let err = parse_trigger("key:ctrl+nonsense").unwrap_err();
        assert!(err.to_string().contains("unknown key name"));
    }

    // ── VK map ───────────────────────────────────────────────────────────────

    #[test]
    fn vk_map_covers_a_through_z() {
        for (i, c) in ('a'..='z').enumerate() {
            assert_eq!(key_name_to_vk(&c.to_string()), Some(0x41 + i as u32), "{c}");
        }
    }

    #[test]
    fn vk_map_covers_f1_through_f24() {
        for n in 1u32..=24 {
            assert_eq!(key_name_to_vk(&format!("f{n}")), Some(0x6F + n), "f{n}");
        }
        assert!(key_name_to_vk("f0").is_none());
        assert!(key_name_to_vk("f25").is_none());
    }

    #[test]
    fn vk_map_is_case_insensitive() {
        assert_eq!(key_name_to_vk("A"), key_name_to_vk("a"));
        assert_eq!(key_name_to_vk("F12"), key_name_to_vk("f12"));
    }

    // ── one_key_trigger ───────────────────────────────────────────────────────

    #[test]
    fn one_key_empty_is_disabled() {
        assert!(parse_one_key_trigger("").unwrap().is_none());
        assert!(parse_one_key_trigger("   ").unwrap().is_none());
    }

    #[test]
    fn one_key_ctrl_alt() {
        assert!(matches!(parse_one_key_trigger("key:ctrl").unwrap(), Some(OneKeyTrigger::Ctrl)));
        assert!(matches!(parse_one_key_trigger("key:alt").unwrap(),  Some(OneKeyTrigger::Alt)));
    }

    #[test]
    fn one_key_function_keys() {
        for n in 1u32..=24 {
            let t = parse_one_key_trigger(&format!("key:f{n}")).unwrap().unwrap();
            match t {
                OneKeyTrigger::Function(vk) => assert_eq!(vk, 0x6F + n, "f{n}"),
                other => panic!("expected Function for f{n}, got {other:?}"),
            }
        }
    }

    #[test]
    fn one_key_rejects_chord() {
        assert!(parse_one_key_trigger("key:ctrl+g").unwrap_err()
            .to_string().contains("cannot be a chord"));
    }

    #[test]
    fn one_key_rejects_bad_inputs() {
        for bad in ["key:a", "key:shift", "key:f0", "key:f25"] {
            assert!(parse_one_key_trigger(bad).is_err(), "{bad} should be rejected");
        }
    }

    // ── vk_to_mod_bit ─────────────────────────────────────────────────────────

    #[test]
    fn mod_bit_recognises_all_modifier_vks() {
        assert_eq!(vk_to_mod_bit(VK_LCONTROL.0 as u32), MOD_CTRL);
        assert_eq!(vk_to_mod_bit(VK_RCONTROL.0 as u32), MOD_CTRL);
        assert_eq!(vk_to_mod_bit(VK_LSHIFT.0 as u32),   MOD_SHIFT);
        assert_eq!(vk_to_mod_bit(VK_RSHIFT.0 as u32),   MOD_SHIFT);
        assert_eq!(vk_to_mod_bit(VK_LMENU.0 as u32),    MOD_ALT);
        assert_eq!(vk_to_mod_bit(VK_RMENU.0 as u32),    MOD_ALT);
        assert_eq!(vk_to_mod_bit(VK_LWIN),               MOD_SUPER);
        assert_eq!(vk_to_mod_bit(VK_RWIN),               MOD_SUPER);
        assert_eq!(vk_to_mod_bit(b'G' as u32),           0);
    }
}
