//! Windows clipboard access and Ctrl+V injection via SendInput.

use anyhow::{anyhow, Result};
use std::ffi::OsStr;
use std::iter;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VIRTUAL_KEY, VK_CONTROL, VK_RETURN, VK_V,
};

const CF_UNICODETEXT: u32 = 13;

pub fn get_clipboard() -> Option<String> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
        let hglobal = HGLOBAL(handle.0);
        let ptr = GlobalLock(hglobal) as *const u16;
        if ptr.is_null() {
            let _ = CloseClipboard();
            return None;
        }
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(hglobal);
        let _ = CloseClipboard();
        Some(text)
    }
}

pub fn set_clipboard(text: &str) -> Result<()> {
    let wide: Vec<u16> = OsStr::new(text)
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let byte_len = wide.len() * 2;

    unsafe {
        OpenClipboard(None).map_err(|e| anyhow!("OpenClipboard: {e}"))?;
        let _ = EmptyClipboard();

        let hmem = GlobalAlloc(GMEM_MOVEABLE, byte_len)
            .map_err(|e| { let _ = CloseClipboard(); anyhow!("GlobalAlloc: {e}") })?;
        let ptr = GlobalLock(hmem) as *mut u16;
        if ptr.is_null() {
            let _ = CloseClipboard();
            return Err(anyhow!("GlobalLock returned null"));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        let _ = GlobalUnlock(hmem);

        SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0))
            .map_err(|e| { let _ = CloseClipboard(); anyhow!("SetClipboardData: {e}") })?;

        let _ = CloseClipboard();
    }
    Ok(())
}

fn key_input(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
    // Chromium-based hosts (Electron/VS Code/Cursor chat) inspect the hardware
    // scan code in lParam and silently drop synthetic keystrokes whose scan
    // code is zero. Map the VK to its scan code so our Ctrl+V is accepted
    // there too, while plain Win32 edits and consoles (which dispatch on VK)
    // keep working unchanged.
    let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } as u16;
    let flags = if key_up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub fn inject_ctrl_v(delay_ms: u32) {
    if delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(delay_ms as u64));
    }

    let inputs = [
        key_input(VK_CONTROL, false),
        key_input(VK_V, false),
        key_input(VK_V, true),
        key_input(VK_CONTROL, true),
    ];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

pub fn inject_enter() {
    let inputs = [
        key_input(VK_RETURN, false),
        key_input(VK_RETURN, true),
    ];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}
