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
use std::process::Command;
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

// ── Terminal detection ────────────────────────────────────────────────────────

// WM_CLASS values for terminal emulators that bind paste to Ctrl+Shift+V
// rather than Ctrl+V (which terminals interpret as literal-next / ^V).
const TERMINAL_CLASSES: &[&str] = &[
    // xterm family
    "Xterm", "UXterm", "Rxvt", "URxvt", "Aterm", "Eterm",
    // GNOME / MATE
    "gnome-terminal-server", "Gnome-terminal", "mate-terminal", "Mate-terminal",
    // XFCE / LXDE
    "Xfce4-terminal", "Xfce4Terminal", "lxterminal", "LXTerminal",
    // KDE
    "konsole", "Konsole",
    // Tilix
    "tilix", "Tilix",
    // GPU terminals
    "kitty", "Kitty", "alacritty", "Alacritty",
    "WezTerm", "wezterm-gui",
    "foot", "foot-server", "Foot",
    // suckless / other
    "st", "st-256color", "Terminology",
];

/// Returns `(is_terminal, window_class)` for the currently focused X11 window.
///
/// Uses `xdotool getactivewindow` then `xprop WM_CLASS` — the same approach
/// as the Python client.  If either tool is missing or fails, returns `(false, "")`.
pub fn detect_terminal_focus() -> (bool, String) {
    let xdotool = which("xdotool");
    let xprop   = which("xprop");
    if xdotool.is_none() || xprop.is_none() {
        return (false, String::new());
    }

    let wid_out = Command::new(xdotool.unwrap())
        .arg("getactivewindow")
        .output()
        .ok();
    let window_id = wid_out
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let window_id = window_id.trim();
    if window_id.is_empty() {
        return (false, String::new());
    }

    let class_out = Command::new(xprop.unwrap())
        .args(["-id", window_id, "WM_CLASS"])
        .output()
        .ok();
    let class_line = class_out
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let is_terminal = TERMINAL_CLASSES
        .iter()
        .any(|&cls| class_line.contains(cls));

    (is_terminal, class_line.trim().to_string())
}

fn which(cmd: &str) -> Option<std::path::PathBuf> {
    std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )
    .map(|dir| dir.join(cmd))
    .find(|p| p.is_file())
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

/// Inject paste. Detects the focused window first — terminals need
/// Ctrl+Shift+V; all other apps accept Ctrl+V.
///
/// Returns `(combo_used, window_class)` for logging.
pub fn inject_paste(delay_ms: u32) -> (&'static str, String) {
    let (is_terminal, win_cls) = detect_terminal_focus();
    let combo = if is_terminal { "ctrl+shift+v" } else { "ctrl+v" };

    if delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(delay_ms as u64));
    }

    if is_terminal {
        sim(&EventType::KeyPress(RdevKey::ControlLeft));
        sim(&EventType::KeyPress(RdevKey::ShiftLeft));
        sim(&EventType::KeyPress(RdevKey::KeyV));
        sim(&EventType::KeyRelease(RdevKey::KeyV));
        sim(&EventType::KeyRelease(RdevKey::ShiftLeft));
        sim(&EventType::KeyRelease(RdevKey::ControlLeft));
    } else {
        sim(&EventType::KeyPress(RdevKey::ControlLeft));
        sim(&EventType::KeyPress(RdevKey::KeyV));
        sim(&EventType::KeyRelease(RdevKey::KeyV));
        sim(&EventType::KeyRelease(RdevKey::ControlLeft));
    }

    (combo, win_cls)
}

pub fn inject_enter() {
    sim(&EventType::KeyPress(RdevKey::Return));
    sim(&EventType::KeyRelease(RdevKey::Return));
}
