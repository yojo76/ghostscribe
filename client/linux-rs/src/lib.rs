//! Library entry point for the GhostScribe Linux push-to-talk client.
//!
//! Exposes the cross-platform modules so integration tests in `tests/` can
//! import them.  Platform-specific modules (hotkey, paste, tray) are exposed
//! unconditionally since the whole crate targets Linux.

pub mod audio;
pub mod config;
pub mod hotkey;
pub mod paste;
pub mod tray;
pub mod upload;
pub mod watcher;
