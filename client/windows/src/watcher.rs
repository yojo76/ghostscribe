//! mtime-polling config-file watcher.
//!
//! We deliberately avoid `notify`/`ReadDirectoryChangesW` for three reasons:
//!
//! 1. Editors routinely save via rename-and-replace, delete-and-create, or
//!    atomic-rename-from-tempfile; all of those generate different event
//!    sequences that a filesystem-event library has to paper over. A simple
//!    `metadata().modified()` poll is immune to the distinction.
//! 2. This is a single-file watcher. The polling thread wakes once per
//!    second, does one `stat()`, and blocks. It is not worth a runtime
//!    dependency.
//! 3. We want to report parse errors *back to the tray*, not swallow them.
//!    Owning the parse step here keeps the error path in the same module.
//!
//! Detected transitions (relative to the last observed state):
//! - file mtime advanced  → `try_reload()` → `Reloaded` / `ParseError`
//! - file disappeared     → `Missing`       (logged, UI can ignore)
//! - file reappeared      → treated as mtime advance on the new file
//!
//! Cancellation: the watcher thread does not own the config store. It
//! holds only a `PathBuf` + a send handle. When the main event loop exits,
//! the proxy's target is gone and `send_event()` returns `Err(_)`; the
//! watcher treats that as a quit signal and returns.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

use crate::config::{self, ClientConfig, ConfigDiff};

/// Events pushed from the watcher thread. The tray event loop turns these
/// into menu state changes and balloon notifications.
#[derive(Debug)]
pub enum WatcherEvent {
    /// File on disk parsed cleanly; diff describes what changed.
    /// `new_config` is the full replacement.
    Reloaded {
        new_config: Box<ClientConfig>,
        diff: ConfigDiff,
    },
    /// File existed but failed to parse or read. Surface to the user;
    /// do not touch the running config.
    ParseError { message: String },
    /// First-seen "file missing" transition after startup. Useful if
    /// the user deleted the config file outright.
    Missing,
}

/// Abstraction so tests can push events into a plain `mpsc::Sender` without
/// building a `tao::EventLoop`.
pub trait EventSink: Send + 'static {
    fn send(&self, event: WatcherEvent) -> Result<(), ()>;
}

impl<F> EventSink for F
where
    F: Fn(WatcherEvent) -> Result<(), ()> + Send + 'static,
{
    fn send(&self, event: WatcherEvent) -> Result<(), ()> {
        (self)(event)
    }
}

/// Adapter that wraps `mpsc::Sender<WatcherEvent>`. Convenient for tests.
pub struct MpscSink(pub mpsc::Sender<WatcherEvent>);
impl EventSink for MpscSink {
    fn send(&self, event: WatcherEvent) -> Result<(), ()> {
        self.0.send(event).map_err(|_| ())
    }
}

/// Poll `path` at `interval` against the given snapshot. Runs on the
/// calling thread. `stop` is checked before every poll so callers can
/// break out deterministically in tests.
///
/// The function is intentionally side-effect-free apart from pushing into
/// `sink`: it does not mutate any shared config store. That responsibility
/// belongs to the main event loop, which is the single writer.
pub fn poll_once(
    path: &Path,
    baseline: &ClientConfig,
    last_mtime: &mut Option<SystemTime>,
    last_was_missing: &mut bool,
    sink: &dyn EventSink,
) -> Result<(), ()> {
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(mtime) => {
            *last_was_missing = false;
            if Some(mtime) == *last_mtime {
                return Ok(());
            }
            *last_mtime = Some(mtime);

            match config::load_from(path) {
                Ok(new_cfg) => {
                    let d = config::diff(baseline, &new_cfg);
                    if d.is_empty() {
                        // Same semantic config; typical for "save with no
                        // edits" or whitespace-only changes.
                        return Ok(());
                    }
                    sink.send(WatcherEvent::Reloaded {
                        new_config: Box::new(new_cfg),
                        diff: d,
                    })
                }
                Err(e) => sink.send(WatcherEvent::ParseError {
                    message: format!("{e:#}"),
                }),
            }
        }
        Err(_) => {
            if !*last_was_missing {
                *last_was_missing = true;
                *last_mtime = None;
                return sink.send(WatcherEvent::Missing);
            }
            Ok(())
        }
    }
}

/// Spawn a long-running watcher thread. `baseline_fn` must return the
/// current live config on each tick so diffs are computed against the
/// *latest* state (otherwise two quick edits would be diffed against the
/// same snapshot and the second would look like a revert).
pub fn spawn<S, F>(path: PathBuf, baseline_fn: F, sink: S) -> thread::JoinHandle<()>
where
    S: EventSink,
    F: Fn() -> ClientConfig + Send + 'static,
{
    thread::spawn(move || {
        let mut last_mtime: Option<SystemTime> = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok();
        let mut last_was_missing = last_mtime.is_none();

        loop {
            let baseline = baseline_fn();
            if poll_once(&path, &baseline, &mut last_mtime, &mut last_was_missing, &sink).is_err() {
                // receiver gone → event loop exited → quit cleanly.
                return;
            }
            thread::sleep(Duration::from_secs(1));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientConfig;
    use std::io::Write;
    use std::sync::mpsc::channel;

    fn write_file(path: &Path, contents: &str) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }

    fn tmp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ghostscribe-watcher-{}", rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn rand_suffix() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{nanos:x}")
    }

    fn baseline() -> ClientConfig {
        // Deliberately mutate from defaults so diffs against the watcher's
        // parsed result produce non-empty results in the happy path.
        let mut cfg = crate::config::load(None).unwrap();
        cfg.server_url = "http://baseline:5005".into();
        cfg
    }

    #[test]
    fn parse_error_surfaces_with_message() {
        let path = tmp_path("bad.toml");
        write_file(&path, "not = valid = toml");

        let (tx, rx) = channel();
        let mut mtime = None;
        let mut missing = false;
        let sink = MpscSink(tx);
        poll_once(&path, &baseline(), &mut mtime, &mut missing, &sink).unwrap();

        match rx.try_recv().unwrap() {
            WatcherEvent::ParseError { message } => assert!(message.contains("parsing")),
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn reload_event_reports_only_changed_keys() {
        let path = tmp_path("good.toml");
        write_file(
            &path,
            r#"server_url = "http://alt:1234"
auto_paste = false
"#,
        );

        let (tx, rx) = channel();
        let mut mtime = None;
        let mut missing = false;
        let sink = MpscSink(tx);
        poll_once(&path, &baseline(), &mut mtime, &mut missing, &sink).unwrap();

        match rx.try_recv().unwrap() {
            WatcherEvent::Reloaded { diff, new_config } => {
                assert!(diff.hot_changed.contains(&"server_url"));
                assert!(diff.hot_changed.contains(&"auto_paste"));
                assert!(diff.cold_changed.is_empty());
                assert_eq!(new_config.server_url, "http://alt:1234");
            }
            other => panic!("expected Reloaded, got {other:?}"),
        }
    }

    #[test]
    fn unchanged_mtime_emits_nothing() {
        let path = tmp_path("stable.toml");
        write_file(&path, r#"server_url = "http://alt:1""#);

        let (tx, rx) = channel();
        let mut mtime = std::fs::metadata(&path).unwrap().modified().ok();
        let mut missing = false;
        let sink = MpscSink(tx);
        poll_once(&path, &baseline(), &mut mtime, &mut missing, &sink).unwrap();

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn missing_file_fires_once_then_goes_quiet() {
        let path = tmp_path("vanished.toml");
        let (tx, rx) = channel();
        let mut mtime = None;
        let mut missing = false;
        let sink = MpscSink(tx);
        // First poll: file does not exist → Missing event.
        poll_once(&path, &baseline(), &mut mtime, &mut missing, &sink).unwrap();
        assert!(matches!(rx.try_recv(), Ok(WatcherEvent::Missing)));
        // Second poll: still missing → silence.
        poll_once(&path, &baseline(), &mut mtime, &mut missing, &sink).unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn no_semantic_change_is_silent() {
        let path = tmp_path("noop.toml");
        write_file(&path, r#"server_url = "http://baseline:5005""#);

        let (tx, rx) = channel();
        let mut mtime = None;
        let mut missing = false;
        let sink = MpscSink(tx);
        poll_once(&path, &baseline(), &mut mtime, &mut missing, &sink).unwrap();
        assert!(rx.try_recv().is_err());
    }
}
