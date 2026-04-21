//! Global push-to-talk hotkey using Windows' low-level keyboard hook.
//!
//! The trigger is configured via `TriggerKeys` (parsed from config).
//! With a modifier: both modifier and key must be held to start recording;
//! releasing either stops it. Without a modifier: press starts, release stops.

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
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

struct HookState {
    tx: Sender<HotkeyEvent>,
    trigger: TriggerKeys,
    modifier_down: AtomicBool,
    key_down: AtomicBool,
    recording: AtomicBool,
}

static STATE: OnceLock<HookState> = OnceLock::new();

fn is_modifier_vk(vk: u32, modifier: Modifier) -> bool {
    match modifier {
        Modifier::Ctrl  => vk == VK_LCONTROL.0 as u32 || vk == VK_RCONTROL.0 as u32,
        Modifier::Shift => vk == VK_LSHIFT.0 as u32   || vk == VK_RSHIFT.0 as u32,
        Modifier::Alt   => vk == VK_LMENU.0 as u32    || vk == VK_RMENU.0 as u32,
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

            let is_main_key = vk == state.trigger.key;
            let is_mod_key  = state.trigger.modifier
                .map(|m| is_modifier_vk(vk, m))
                .unwrap_or(false);

            if is_mod_key {
                if is_down { state.modifier_down.store(true,  Ordering::SeqCst); }
                else if is_up { state.modifier_down.store(false, Ordering::SeqCst); }
            } else if is_main_key {
                if is_down { state.key_down.store(true,  Ordering::SeqCst); }
                else if is_up { state.key_down.store(false, Ordering::SeqCst); }
            }

            let recording = state.recording.load(Ordering::SeqCst);

            let should_start = match state.trigger.modifier {
                Some(_) => state.modifier_down.load(Ordering::SeqCst)
                        && state.key_down.load(Ordering::SeqCst),
                None    => state.key_down.load(Ordering::SeqCst),
            };

            let should_stop = match state.trigger.modifier {
                Some(_) => !state.modifier_down.load(Ordering::SeqCst)
                        || !state.key_down.load(Ordering::SeqCst),
                None    => !state.key_down.load(Ordering::SeqCst),
            };

            if should_start && !recording {
                state.recording.store(true, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Press);
            } else if should_stop && recording && is_up && (is_main_key || is_mod_key) {
                state.recording.store(false, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Release);
            }
        }
    }
    CallNextHookEx(HHOOK::default(), n_code, w_param, l_param)
}

/// Installs the low-level keyboard hook and runs the Windows message pump on
/// the calling thread. Blocks until `WM_QUIT`. Must run on a dedicated thread.
pub fn run_hook(tx: Sender<HotkeyEvent>, trigger: TriggerKeys) -> Result<()> {
    let state = HookState {
        tx,
        trigger,
        modifier_down: AtomicBool::new(false),
        key_down: AtomicBool::new(false),
        recording: AtomicBool::new(false),
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
