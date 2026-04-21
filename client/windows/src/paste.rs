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
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_V,
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

pub fn inject_ctrl_v(delay_ms: u32) {
    if delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(delay_ms as u64));
    }

    let ctrl_down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_CONTROL,
                wScan: 0,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let v_down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_V,
                wScan: 0,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let v_up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_V,
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let ctrl_up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_CONTROL,
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    let inputs = [ctrl_down, v_down, v_up, ctrl_up];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}
