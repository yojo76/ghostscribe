//! Global Ctrl+G push-to-talk hotkey using Windows' low-level keyboard hook.
//!
//! Press = recording starts only once **both** Ctrl (either Ctrl_L or Ctrl_R)
//! and the `G` key are held down.
//! Release of EITHER key stops the recording and triggers the upload.

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_G, VK_LCONTROL, VK_RCONTROL,
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

struct HookState {
    tx: Sender<HotkeyEvent>,
    ctrl_down: AtomicBool,
    g_down: AtomicBool,
    recording: AtomicBool,
}

static STATE: OnceLock<HookState> = OnceLock::new();

unsafe extern "system" fn keyboard_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code >= 0 {
        let kb = &*(l_param.0 as *const KBDLLHOOKSTRUCT);
        let vk = kb.vkCode;
        let msg = w_param.0 as u32;

        if let Some(state) = STATE.get() {
            let is_ctrl = vk == VK_LCONTROL.0 as u32
                || vk == VK_RCONTROL.0 as u32
                || vk == VK_CONTROL.0 as u32;
            let is_g = vk == VK_G.0 as u32;

            let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

            if is_ctrl {
                if is_down {
                    state.ctrl_down.store(true, Ordering::SeqCst);
                } else if is_up {
                    state.ctrl_down.store(false, Ordering::SeqCst);
                }
            } else if is_g {
                if is_down {
                    state.g_down.store(true, Ordering::SeqCst);
                } else if is_up {
                    state.g_down.store(false, Ordering::SeqCst);
                }
            }

            let both_held = state.ctrl_down.load(Ordering::SeqCst)
                && state.g_down.load(Ordering::SeqCst);
            let recording = state.recording.load(Ordering::SeqCst);

            if both_held && !recording {
                state.recording.store(true, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Press);
            } else if !both_held && recording && (is_ctrl || is_g) && is_up {
                state.recording.store(false, Ordering::SeqCst);
                let _ = state.tx.send(HotkeyEvent::Release);
            }
        }
    }
    CallNextHookEx(HHOOK::default(), n_code, w_param, l_param)
}

/// Installs the low-level keyboard hook and runs the Windows message pump on
/// the calling thread. Blocks until `WM_QUIT`. Must run on a thread whose only
/// job is to pump messages (the hook callback runs on the thread that
/// installed the hook).
pub fn run_hook(tx: Sender<HotkeyEvent>) -> Result<()> {
    let state = HookState {
        tx,
        ctrl_down: AtomicBool::new(false),
        g_down: AtomicBool::new(false),
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
