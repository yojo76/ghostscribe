//! Linux system-tray glue.
//!
//! Uses the same `tray-icon` + `tao` crates as the Windows client; on Linux
//! `tray-icon` uses the libappindicator3 backend.  The menu structure, state
//! machine, and icon generation are identical to the Windows version.
//!
//! ## Build requirements
//!
//! ```sh
//! sudo apt install libappindicator3-dev libgtk-3-dev
//! ```

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

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
            TrayState::Idle      => [ 40, 180,  40, 255],
            TrayState::Recording => [220,  40,  40, 255],
            TrayState::Uploading => [ 40, 100, 220, 255],
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

fn make_icon_rgba(state: TrayState) -> Vec<u8> {
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let [r, g, b, _a] = state.tint();
    let cx = (SIZE as f32 - 1.0) / 2.0;
    let cy = (SIZE as f32 - 1.0) / 2.0;
    let radius = 13.0_f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let alpha = if d <= radius - 0.5 {
                255.0
            } else if d >= radius + 0.5 {
                0.0
            } else {
                (radius + 0.5 - d) * 255.0
            };
            if alpha > 0.0 {
                let i = (y * SIZE + x) * 4;
                rgba[i]     = r;
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

pub struct Tray {
    icon: TrayIcon,
    state: TrayState,
    _menu: Menu,
    log_item: CheckMenuItem,
}

impl Tray {
    pub fn new(config_path: Option<PathBuf>) -> Result<Self> {
        let menu = Menu::new();
        let has_path = config_path.is_some();
        let log_item = CheckMenuItem::with_id(id::TOGGLE_LOG, "Logging", true, true, None);

        menu.append_items(&[
            &MenuItem::with_id(id::EDIT_CONFIG,   "Edit config…",              true,     None),
            &MenuItem::with_id(id::REVEAL_CONFIG, "Reveal config in Files",    has_path, None),
            &MenuItem::with_id(id::RELOAD_CONFIG, "Reload now",                true,     None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(id::SHOW_LOG,      "Show log",                  true,     None),
            &log_item,
            &MenuItem::with_id(id::RESTART,       "Restart client",            true,     None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(id::ABOUT,         "About GhostScribe",         true,     None),
            &MenuItem::with_id(id::QUIT,          "Quit",                      true,     None),
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

    pub fn set_tooltip_suffix(&self, suffix: &str) {
        let tip = format!("{} — {}", self.state.tooltip(), suffix);
        let _ = self.icon.set_tooltip(Some(tip));
    }

    pub fn is_logging_enabled(&self) -> bool {
        self.log_item.is_checked()
    }
}

pub fn spawn_menu_forwarder(tx: Sender<MenuAction>) {
    std::thread::spawn(move || {
        let rx = MenuEvent::receiver();
        while let Ok(ev) = rx.recv() {
            if let Some(action) = MenuAction::from_id(ev.id.as_ref()) {
                if tx.send(action).is_err() {
                    return;
                }
            }
        }
    });
}

pub type SharedTray = Arc<Mutex<Option<Tray>>>;

/// Show a blocking error dialog. Tries `zenity` first; falls back to
/// `xmessage`; on failure logs to stderr. Unlike the Windows version this
/// does not block the GTK main loop — call it only from a non-GUI thread.
pub fn show_error_box(title: &str, body: &str) {
    use std::process::Command;
    let shown = Command::new("zenity")
        .args(["--error", "--title", title, "--text", body, "--no-wrap"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !shown {
        let msg = format!("{title}: {body}");
        let _ = Command::new("xmessage").args(["-center", &msg]).status();
    }
}

pub type MenuActionSender   = Sender<MenuAction>;
pub type MenuActionReceiver = Receiver<MenuAction>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_action_round_trip_covers_all_ids() {
        for s in [
            id::EDIT_CONFIG, id::REVEAL_CONFIG, id::RELOAD_CONFIG,
            id::SHOW_LOG, id::TOGGLE_LOG, id::RESTART, id::ABOUT, id::QUIT,
        ] {
            assert!(MenuAction::from_id(s).is_some(), "missing mapping for {s}");
        }
        assert!(MenuAction::from_id("nope").is_none());
    }

    #[test]
    fn icon_rgba_is_32x32_and_has_alpha() {
        let rgba = make_icon_rgba(TrayState::Recording);
        assert_eq!(rgba.len(), 32 * 32 * 4);
        assert!(rgba.chunks_exact(4).any(|px| px[3] == 255));
        assert!(rgba.chunks_exact(4).any(|px| px[3] == 0));
    }

    #[test]
    fn each_state_has_a_unique_tint() {
        let tints = [
            TrayState::Idle.tint(), TrayState::Recording.tint(),
            TrayState::Uploading.tint(), TrayState::Error.tint(),
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
