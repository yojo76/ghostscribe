//! Global push-to-talk hotkey using Windows' low-level keyboard hook.
//!
//! The trigger is configured via `TriggerKeys` (parsed from config).
//! With a modifier: both modifier and key must be held to start recording;
//! releasing either stops it. Without a modifier: press starts, release stops.

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
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

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

#[derive(Debug, Clone, Copy)]
pub struct TriggerKeys {
    pub modifier: Option<Modifier>,
    pub key: u32,
}

/// Single-key push-to-talk trigger. Intentionally restricted to keys that
/// don't produce text on their own (modifiers or F-keys) to avoid hijacking
/// normal typing. No chords — use `TriggerKeys` for those.
#[derive(Debug, Clone, Copy)]
pub enum OneKeyTrigger {
    Ctrl,
    Alt,
    /// Raw VK code (e.g. 0x70 for F1).
    Function(u32),
}

pub fn parse_one_key_trigger(s: &str) -> Result<Option<OneKeyTrigger>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let rest = s
        .strip_prefix("key:")
        .ok_or_else(|| anyhow!("one_key_trigger must start with 'key:' (got {s:?})"))?;
    if rest.contains('+') {
        return Err(anyhow!(
            "one_key_trigger cannot be a chord (got {s:?}); use trigger= for chords"
        ));
    }
    let lower = rest.to_lowercase();
    match lower.as_str() {
        "ctrl" => Ok(Some(OneKeyTrigger::Ctrl)),
        "alt" => Ok(Some(OneKeyTrigger::Alt)),
        _ => {
            if let Some(n) = lower.strip_prefix('f') {
                if let Ok(num) = n.parse::<u32>() {
                    if (1..=24).contains(&num) {
                        return Ok(Some(OneKeyTrigger::Function(0x6F + num)));
                    }
                }
            }
            Err(anyhow!(
                "one_key_trigger must be one of: key:ctrl, key:alt, key:f1..key:f24 (got {s:?})"
            ))
        }
    }
}

pub fn parse_trigger(s: &str) -> Result<TriggerKeys> {
    let s = s.trim();
    let rest = s
        .strip_prefix("key:")
        .ok_or_else(|| anyhow!("trigger must start with 'key:' (got {s:?})"))?;

    let (modifier, key_name) = if let Some(plus) = rest.find('+') {
        let mod_str = &rest[..plus];
        let key_str = &rest[plus + 1..];
        let modifier = match mod_str {
            "ctrl" => Modifier::Ctrl,
            "shift" => Modifier::Shift,
            "alt" => Modifier::Alt,
            other => return Err(anyhow!("unknown modifier {other:?}; use ctrl, shift, or alt")),
        };
        (Some(modifier), key_str)
    } else {
        (None, rest)
    };

    let key = key_name_to_vk(key_name)
        .ok_or_else(|| anyhow!("unknown key name {key_name:?}"))?;

    Ok(TriggerKeys { modifier, key })
}

fn key_name_to_vk(name: &str) -> Option<u32> {
    let name = name.to_lowercase();
    // Letters a-z → VK 0x41–0x5A
    if name.len() == 1 {
        let c = name.chars().next()?;
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    // Function keys f1–f24 → 0x70–0x87
    if let Some(n) = name.strip_prefix('f') {
        if let Ok(num) = n.parse::<u32>() {
            if (1..=24).contains(&num) {
                return Some(0x6F + num);
            }
        }
    }
    match name.as_str() {
        "pause"     => Some(0x13),
        "capslock"  => Some(0x14),
        "escape"    => Some(0x1B),
        "space"     => Some(0x20),
        "insert"    => Some(0x2D),
        "delete"    => Some(0x2E),
        "home"      => Some(0x24),
        "end"       => Some(0x23),
        "pageup"    => Some(0x21),
        "pagedown"  => Some(0x22),
        "left"      => Some(0x25),
        "up"        => Some(0x26),
        "right"     => Some(0x27),
        "down"      => Some(0x28),
        "scroll"    => Some(0x91),
        "numlock"   => Some(0x90),
        "printscreen" => Some(0x2C),
        _ => None,
    }
}

const MODE_IDLE: u8 = 0;
const MODE_CHORD: u8 = 1;
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

fn is_modifier_vk(vk: u32, modifier: Modifier) -> bool {
    match modifier {
        Modifier::Ctrl  => vk == VK_LCONTROL.0 as u32 || vk == VK_RCONTROL.0 as u32,
        Modifier::Shift => vk == VK_LSHIFT.0 as u32   || vk == VK_RSHIFT.0 as u32,
        Modifier::Alt   => vk == VK_LMENU.0 as u32    || vk == VK_RMENU.0 as u32,
    }
}

fn matches_one_key(vk: u32, trig: OneKeyTrigger) -> bool {
    match trig {
        OneKeyTrigger::Ctrl => is_modifier_vk(vk, Modifier::Ctrl),
        OneKeyTrigger::Alt => is_modifier_vk(vk, Modifier::Alt),
        OneKeyTrigger::Function(f) => vk == f,
    }
}

unsafe extern "system" fn keyboard_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code >= 0 {
        let kb = &*(l_param.0 as *const KBDLLHOOKSTRUCT);
        let vk = kb.vkCode;
        let msg = w_param.0 as u32;

        if let Some(state) = STATE.get() {
            let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let is_up   = msg == WM_KEYUP   || msg == WM_SYSKEYUP;

            let is_chord_main = vk == state.trigger.key;
            let is_chord_mod  = state.trigger.modifier
                .map(|m| is_modifier_vk(vk, m))
                .unwrap_or(false);
            let is_one_key = state.one_key
                .map(|t| matches_one_key(vk, t))
                .unwrap_or(false);

            if is_down {
                if is_chord_mod  { state.chord_mod_down.store(true, Ordering::SeqCst); }
                if is_chord_main { state.chord_key_down.store(true, Ordering::SeqCst); }
            } else if is_up {
                if is_chord_mod  { state.chord_mod_down.store(false, Ordering::SeqCst); }
                if is_chord_main { state.chord_key_down.store(false, Ordering::SeqCst); }
            }

            let mode = state.mode.load(Ordering::SeqCst);

            // First-to-start-wins: chord is checked before one-key, so a fully
            // satisfied chord always takes precedence over a modifier also
            // configured as one_key_trigger.
            match (mode, is_down, is_up) {
                (MODE_IDLE, true, _) => {
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
                (MODE_CHORD, _, true) => {
                    if is_chord_main || is_chord_mod {
                        state.mode.store(MODE_IDLE, Ordering::SeqCst);
                        let _ = state.tx.send(HotkeyEvent::Release);
                    }
                }
                (MODE_ONE_KEY, true, _) => {
                    // Neutral set: the one-key itself, plus any key that is
                    // part of the configured chord (main OR modifier). Pressing
                    // anything else mid-recording cancels the take.
                    let neutral = is_one_key || is_chord_main || is_chord_mod;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_ctrl_g() {
        let t = parse_trigger("key:ctrl+g").unwrap();
        assert!(matches!(t.modifier, Some(Modifier::Ctrl)));
        assert_eq!(t.key, b'G' as u32);
    }

    #[test]
    fn parse_key_f12() {
        let t = parse_trigger("key:f12").unwrap();
        assert!(t.modifier.is_none());
        assert_eq!(t.key, 0x7B); // VK_F12
    }

    #[test]
    fn parse_multi_modifier_is_not_supported() {
        // Windows parser only accepts a single modifier (unlike the Linux client).
        // Locked in as a known divergence — upgrade this test if multi-mod lands.
        let err = parse_trigger("key:ctrl+shift+space").unwrap_err();
        assert!(err.to_string().contains("unknown key name"));
    }

    #[test]
    fn parse_bare_letter_no_modifier() {
        let t = parse_trigger("key:a").unwrap();
        assert!(t.modifier.is_none());
        assert_eq!(t.key, b'A' as u32);
    }

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

    #[test]
    fn vk_map_covers_a_through_z() {
        for (i, c) in ('a'..='z').enumerate() {
            let vk = key_name_to_vk(&c.to_string()).unwrap();
            assert_eq!(vk, 0x41 + i as u32, "letter {c}");
        }
    }

    #[test]
    fn vk_map_covers_f1_through_f24() {
        for n in 1u32..=24 {
            let vk = key_name_to_vk(&format!("f{n}")).unwrap();
            assert_eq!(vk, 0x6F + n, "f{n}");
        }
        assert!(key_name_to_vk("f25").is_none());
        assert!(key_name_to_vk("f0").is_none());
    }

    #[test]
    fn vk_map_covers_special_keys() {
        let cases = [
            ("pause", 0x13u32),
            ("capslock", 0x14),
            ("escape", 0x1B),
            ("space", 0x20),
            ("insert", 0x2D),
            ("delete", 0x2E),
            ("home", 0x24),
            ("end", 0x23),
            ("pageup", 0x21),
            ("pagedown", 0x22),
            ("left", 0x25),
            ("up", 0x26),
            ("right", 0x27),
            ("down", 0x28),
            ("scroll", 0x91),
            ("numlock", 0x90),
            ("printscreen", 0x2C),
        ];
        for (name, expected) in cases {
            assert_eq!(key_name_to_vk(name), Some(expected), "{name}");
        }
    }

    #[test]
    fn vk_map_is_case_insensitive() {
        assert_eq!(key_name_to_vk("A"), key_name_to_vk("a"));
        assert_eq!(key_name_to_vk("F12"), key_name_to_vk("f12"));
        assert_eq!(key_name_to_vk("SPACE"), key_name_to_vk("space"));
    }

    #[test]
    fn vk_map_rejects_unknown_name() {
        assert!(key_name_to_vk("unicorn").is_none());
        assert!(key_name_to_vk("").is_none());
    }

    #[test]
    fn parse_one_key_empty_is_disabled() {
        assert!(parse_one_key_trigger("").unwrap().is_none());
        assert!(parse_one_key_trigger("   ").unwrap().is_none());
    }

    #[test]
    fn parse_one_key_ctrl_alt() {
        assert!(matches!(
            parse_one_key_trigger("key:ctrl").unwrap(),
            Some(OneKeyTrigger::Ctrl)
        ));
        assert!(matches!(
            parse_one_key_trigger("key:alt").unwrap(),
            Some(OneKeyTrigger::Alt)
        ));
    }

    #[test]
    fn parse_one_key_function_keys() {
        for n in 1u32..=24 {
            let t = parse_one_key_trigger(&format!("key:f{n}")).unwrap().unwrap();
            match t {
                OneKeyTrigger::Function(vk) => assert_eq!(vk, 0x6F + n, "f{n}"),
                other => panic!("expected Function for f{n}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_one_key_is_case_insensitive() {
        assert!(matches!(
            parse_one_key_trigger("KEY:CTRL").unwrap(),
            Some(OneKeyTrigger::Ctrl)
        ));
        assert!(matches!(
            parse_one_key_trigger("key:F12").unwrap(),
            Some(OneKeyTrigger::Function(0x7B))
        ));
    }

    #[test]
    fn parse_one_key_rejects_missing_prefix() {
        let err = parse_one_key_trigger("ctrl").unwrap_err();
        assert!(err.to_string().contains("must start with 'key:'"));
    }

    #[test]
    fn parse_one_key_rejects_chord() {
        let err = parse_one_key_trigger("key:ctrl+g").unwrap_err();
        assert!(err.to_string().contains("cannot be a chord"));
    }

    #[test]
    fn parse_one_key_rejects_letters_digits_shift() {
        // Letters/digits would hijack typing; shift would trigger on every
        // capital letter. F25 is out of VK range.
        for bad in ["key:a", "key:g", "key:1", "key:shift", "key:f0", "key:f25"] {
            assert!(parse_one_key_trigger(bad).is_err(), "{bad} should be rejected");
        }
    }
}

/// Installs the low-level keyboard hook and runs the Windows message pump on
/// the calling thread. Blocks until `WM_QUIT`. Must run on a dedicated thread.
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

    unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0)
            .map_err(|e| anyhow!("SetWindowsHookExW failed: {e}"))?;

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = UnhookWindowsHookEx(hook);
    }

    Ok(())
}
