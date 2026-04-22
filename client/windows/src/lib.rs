//! Library entry point for the GhostScribe Windows push-to-talk client.
//!
//! Exposes the cross-platform modules so integration tests in `tests/` can
//! import them. Windows-only modules (hotkey hook, clipboard/SendInput) are
//! gated out on non-Windows targets so the library still builds for test
//! runners that only exercise the portable surface.

pub mod audio;
pub mod config;
pub mod upload;

#[cfg(target_os = "windows")]
pub mod hotkey;

#[cfg(target_os = "windows")]
pub mod paste;
