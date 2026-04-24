//! Clipboard access and keystroke injection for Linux.
//!
//! Clipboard: `arboard` (X11/Wayland, maintains selection in a background
//! thread so the data survives the function returning).
//!
//! Keystroke injection: `rdev::simulate` (X11 XTest extension — no root
//! required on a normal desktop).  A small inter-event sleep is required
//! because the X server queues events; without it a fast target app may
//! process the key before the clipboard data is ready.

use anyhow::{anyhow, Result};
use rdev::{simulate, EventType, Key as RdevKey, SimulateError};
use std::time::Duration;

// ── Clipboard ─────────────────────────────────────────────────────────────────

pub fn get_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

pub fn set_clipboard(text: &str) -> Result<()> {
    arboard::Clipboard::new()
        .map_err(|e| anyhow!("open clipboard: {e}"))?
        .set_text(text)
        .map_err(|e| anyhow!("set clipboard: {e}"))
}

// ── Key injection ─────────────────────────────────────────────────────────────

/// Minimum inter-event gap. The X server queues events; without a small sleep
/// some applications miss the key-up before acting on the key-down.
const SIM_DELAY: Duration = Duration::from_millis(10);

fn sim(event_type: &EventType) {
    if let Err(SimulateError) = simulate(event_type) {
        eprintln!("[paste] rdev::simulate failed for {event_type:?}");
    }
    std::thread::sleep(SIM_DELAY);
}

pub fn inject_ctrl_v(delay_ms: u32) {
    if delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(delay_ms as u64));
    }
    sim(&EventType::KeyPress(RdevKey::ControlLeft));
    sim(&EventType::KeyPress(RdevKey::KeyV));
    sim(&EventType::KeyRelease(RdevKey::KeyV));
    sim(&EventType::KeyRelease(RdevKey::ControlLeft));
}

pub fn inject_enter() {
    sim(&EventType::KeyPress(RdevKey::Return));
    sim(&EventType::KeyRelease(RdevKey::Return));
}
