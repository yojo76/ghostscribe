//! Windows system-tray glue.
//!
//! Owns the tray icon, its state-driven icon texture, the right-click menu,
//! and the mapping from menu clicks to a typed [`MenuAction`] enum that the
//! main event loop consumes. All icons are generated procedurally at startup
//! (32×32 RGBA, filled circle, per-state tint) so the binary carries no
//! image assets. This trades artistic fidelity for a self-contained build.
//!
//! User-visible feedback policy:
//! * Icon colour is the primary state indicator (idle/rec/upload/error).
//! * The tooltip is a short one-liner kept in sync with the last event.
//! * Parse errors (from the config watcher) surface as a blocking
//!   `MessageBoxW`: the user needs to read and fix the TOML, and a transient
//!   balloon is too easy to miss.
//! * Informational events ("Config reloaded") are tooltip-only, no popup.
//!
//! This is a deliberate simplification vs. the original design, which called
//! for `Shell_NotifyIconW NIF_INFO` balloons. The `tray-icon` crate owns the
//! internal `NOTIFYICONDATAW` we'd need to push balloons through, and
//! reaching into it would be more ceremony than value for a first cut.

#![cfg(target_os = "windows")]

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

/// Live operational state. Drives tooltip + icon colour. Kept deliberately
/// narrow: we do not want the UI to reflect *every* internal transition,
/// only the ones a user would recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Recording,
    Uploading,
    Error,
}

impl TrayState {
    fn tint(self) -> [u8; 4] {
        match self {
            // Green dot: matches the Linux tray idle colour.
            TrayState::Idle      => [ 40, 180,  40, 255],
            // Red = active capture.
            TrayState::Recording => [220,  40,  40, 255],
            // Blue = network in flight.
            TrayState::Uploading => [ 40, 100, 220, 255],
            // Amber = something went wrong; still running, user action
            // recommended. Red would collide with Recording.
            TrayState::Error     => [240, 180,  40, 255],
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            TrayState::Idle      => "GhostScribe — idle",
            TrayState::Recording => "GhostScribe — recording…",
            TrayState::Uploading => "GhostScribe — uploading…",
            TrayState::Error     => "GhostScribe — error (see log)",
        }
    }
}

/// Stable string IDs for menu items. Kept as constants so the main event
/// loop can match on them without sharing `MenuId` instances across threads.
pub mod id {
    pub const EDIT_CONFIG: &str    = "edit-config";
    pub const REVEAL_CONFIG: &str  = "reveal-config";
    pub const RELOAD_CONFIG: &str  = "reload-config";
    pub const SHOW_LOG: &str       = "show-log";
    pub const TOGGLE_LOG: &str     = "toggle-log";
    pub const RESTART: &str        = "restart";
    pub const ABOUT: &str          = "about";
    pub const QUIT: &str           = "quit";
}

/// Parsed menu click. A string enum would be equally valid, but keeping
/// this decoupled from `MenuEvent` means the main loop doesn't have to
/// import the `tray-icon` crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    EditConfig,
    RevealConfig,
    ReloadConfig,
    ShowLog,
    ToggleLog,
    Restart,
    About,
    Quit,
}

impl MenuAction {
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            id::EDIT_CONFIG    => Some(MenuAction::EditConfig),
            id::REVEAL_CONFIG  => Some(MenuAction::RevealConfig),
            id::RELOAD_CONFIG  => Some(MenuAction::ReloadConfig),
            id::SHOW_LOG       => Some(MenuAction::ShowLog),
            id::TOGGLE_LOG     => Some(MenuAction::ToggleLog),
            id::RESTART        => Some(MenuAction::Restart),
            id::ABOUT          => Some(MenuAction::About),
            id::QUIT           => Some(MenuAction::Quit),
            _ => None,
        }
    }
}

/// Generate a 32×32 RGBA icon: transparent background, filled circle in
/// the state colour. Simple antialiased edge so the icon doesn't look
/// jagged in the taskbar.
fn make_icon_rgba(state: TrayState) -> Vec<u8> {
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let [r, g, b, _a] = state.tint();
    // Circle centred on the pixel grid; radius chosen so ~1px margin.
    let cx = (SIZE as f32 - 1.0) / 2.0;
    let cy = (SIZE as f32 - 1.0) / 2.0;
    let radius = 13.0_f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            // 1px linear alpha ramp at the circle edge for mild AA.
            let alpha = if d <= radius - 0.5 {
                255.0
            } else if d >= radius + 0.5 {
                0.0
            } else {
                (radius + 0.5 - d) * 255.0
            };
            if alpha > 0.0 {
                let i = (y * SIZE + x) * 4;
                rgba[i] = r;
                rgba[i + 1] = g;
                rgba[i + 2] = b;
                rgba[i + 3] = alpha.round() as u8;
            }
        }
    }
    rgba
}

fn make_icon(state: TrayState) -> Result<Icon> {
    let rgba = make_icon_rgba(state);
    Icon::from_rgba(rgba, 32, 32).map_err(|e| anyhow!("build icon: {e}"))
}

/// Owning handle to the tray icon. Construction must happen on a thread
/// that pumps Win32 messages (we rely on the calling thread for that;
/// typically the `tao::EventLoop` on the main thread).
pub struct Tray {
    icon: TrayIcon,
    state: TrayState,
    /// Retained to keep the menu alive for the lifetime of the tray; the
    /// `TrayIcon` borrows into it. Never read after construction.
    _menu: Menu,
    /// Retained so the caller can read back the auto-toggled check state
    /// after a `ToggleLog` menu event fires.
    log_item: CheckMenuItem,
}

impl Tray {
    /// Build the tray icon + menu. `config_path` is shown in the About
    /// dialog and used by the tray's built-in "reveal" handler.
    pub fn new(config_path: Option<PathBuf>) -> Result<Self> {
        let menu = Menu::new();
        let has_path = config_path.is_some();
        let log_item = CheckMenuItem::with_id(id::TOGGLE_LOG, "Logging", true, true, None);

        menu.append_items(&[
            &MenuItem::with_id(id::EDIT_CONFIG,   "Edit config…",          true,  None),
            &MenuItem::with_id(id::REVEAL_CONFIG, "Reveal config in Explorer", has_path, None),
            &MenuItem::with_id(id::RELOAD_CONFIG, "Reload now",            true,  None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(id::SHOW_LOG,      "Show log",              true,  None),
            &log_item,
            &MenuItem::with_id(id::RESTART,       "Restart client",        true,  None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(id::ABOUT,         "About GhostScribe",     true,  None),
            &MenuItem::with_id(id::QUIT,          "Quit",                  true,  None),
        ])
        .map_err(|e| anyhow!("building tray menu: {e}"))?;

        let icon = TrayIconBuilder::new()
            .with_tooltip(TrayState::Idle.tooltip())
            .with_icon(make_icon(TrayState::Idle)?)
            .with_menu(Box::new(menu.clone()))
            .build()
            .map_err(|e| anyhow!("building tray icon: {e}"))?;

        Ok(Self { icon, state: TrayState::Idle, _menu: menu, log_item })
    }

    pub fn set_state(&mut self, state: TrayState) -> Result<()> {
        if self.state == state {
            return Ok(());
        }
        self.state = state;
        self.icon
            .set_icon(Some(make_icon(state)?))
            .map_err(|e| anyhow!("set icon: {e}"))?;
        let _ = self.icon.set_tooltip(Some(state.tooltip()));
        Ok(())
    }

    /// Override the tooltip without changing state. Useful for appending
    /// ad-hoc context like the name of a reloaded file.
    pub fn set_tooltip_suffix(&self, suffix: &str) {
        let tip = format!("{} — {}", self.state.tooltip(), suffix);
        let _ = self.icon.set_tooltip(Some(tip));
    }

    /// Read the check state that `muda` auto-toggled on the last click.
    /// Call this immediately after receiving `MenuAction::ToggleLog` to sync
    /// the application's logging flag.
    pub fn is_logging_enabled(&self) -> bool {
        self.log_item.is_checked()
    }
}

/// Spawn a small thread that forwards `tray_icon::menu::MenuEvent::receiver`
/// (a global crossbeam channel) into an application-typed sender.
///
/// `tray-icon` deliberately exposes menu clicks via a global receiver rather
/// than a per-icon callback; polling it on a dedicated thread keeps the main
/// event loop free of library-specific types.
pub fn spawn_menu_forwarder(tx: Sender<MenuAction>) {
    std::thread::spawn(move || {
        let rx = MenuEvent::receiver();
        while let Ok(ev) = rx.recv() {
            if let Some(action) = MenuAction::from_id(ev.id.as_ref()) {
                if tx.send(action).is_err() {
                    // main loop gone; quit.
                    return;
                }
            }
        }
    });
}

/// Tiny wrapper around a `Mutex<Option<Tray>>` so helpers that outlive the
/// construction scope can still address the icon. The tray itself is not
/// `Send`, so this lives on the main thread; helpers on other threads must
/// route through a channel, not touch the tray directly.
pub type SharedTray = Arc<Mutex<Option<Tray>>>;

/// Pop a blocking message box on the calling thread. Used for config
/// parse errors, where the user needs to read the diagnostic before the
/// client resumes with the previous config.
pub fn show_error_box(title: &str, body: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let title_w: Vec<u16> = title.encode_utf16().chain([0]).collect();
    let body_w: Vec<u16> = body.encode_utf16().chain([0]).collect();
    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR(body_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Channel alias kept here so downstream modules don't have to spell out
/// the Rust type names repeatedly.
pub type MenuActionSender = Sender<MenuAction>;
pub type MenuActionReceiver = Receiver<MenuAction>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_action_round_trip_covers_all_ids() {
        for s in [
            id::EDIT_CONFIG,
            id::REVEAL_CONFIG,
            id::RELOAD_CONFIG,
            id::SHOW_LOG,
            id::TOGGLE_LOG,
            id::RESTART,
            id::ABOUT,
            id::QUIT,
        ] {
            assert!(MenuAction::from_id(s).is_some(), "missing mapping for {s}");
        }
        assert!(MenuAction::from_id("not-a-real-id").is_none());
    }

    #[test]
    fn icon_rgba_is_32x32_and_has_alpha() {
        let rgba = make_icon_rgba(TrayState::Recording);
        assert_eq!(rgba.len(), 32 * 32 * 4);
        let any_opaque = rgba.chunks_exact(4).any(|px| px[3] == 255);
        let any_transparent = rgba.chunks_exact(4).any(|px| px[3] == 0);
        assert!(any_opaque, "icon should have opaque pixels");
        assert!(any_transparent, "icon should have transparent pixels (circle != rectangle)");
    }

    #[test]
    fn each_state_has_a_unique_tint() {
        let tints = [
            TrayState::Idle.tint(),
            TrayState::Recording.tint(),
            TrayState::Uploading.tint(),
            TrayState::Error.tint(),
        ];
        for i in 0..tints.len() {
            for j in (i + 1)..tints.len() {
                assert_ne!(tints[i], tints[j], "states {i} and {j} share a tint");
            }
        }
    }

    #[test]
    fn idle_tint_is_green() {
        let [r, g, b, _] = TrayState::Idle.tint();
        assert!(g > r && g > b, "idle tint should be greenish");
    }
}
