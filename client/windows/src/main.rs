//! GhostScribe Windows push-to-talk client.
//!
//! Hold the configured trigger (default Ctrl+G) to record from the microphone.
//! Release either key to encode WAV, POST to the server, and paste the transcript.
//!
//! Two run modes:
//! * **Tray** (default): shows a system-tray icon, supports in-place config
//!   editing with validation, runs until the user picks Quit. Implies
//!   `--detach` unless the env var `GHOSTSCRIBE_DETACHED=1` is set, so that
//!   double-clicking the exe hides the console window.
//! * **Headless** (`--no-tray`): blocks on the hotkey channel, logs to stderr.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arc_swap::ArcSwap;

use ghostscribe_client::audio::{self, Recorder};
use ghostscribe_client::config::{self, ClientConfig};
use ghostscribe_client::hotkey::{parse_one_key_trigger, parse_trigger, run_hook, HotkeyEvent};
use ghostscribe_client::watcher::{self, WatcherEvent};
use ghostscribe_client::{paste, upload};

// Win32 process creation flags. Hand-coded to avoid pulling another `windows`
// crate feature just for two constants.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Env var set on the detached child so we don't double-detach in a loop.
const ENV_DETACHED: &str = "GHOSTSCRIBE_DETACHED";

/// Auto-chunk interval: upload a partial transcript every 2 minutes while
/// recording continues.
const CHUNK_INTERVAL_S: u64 = 2 * 60;

/// Controls whether operational log lines are written. Toggled via the tray
/// "Logging" checkbox; defaults to on.
static LOGGING_ENABLED: AtomicBool = AtomicBool::new(true);

macro_rules! logln {
    ($($arg:tt)*) => {
        if LOGGING_ENABLED.load(Ordering::Relaxed) {
            eprintln!($($arg)*);
        }
    };
}

// ── "do it now" ──────────────────────────────────────────────────────────────

fn is_do_it_now(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    let bare = lower.trim_end_matches(|c: char| !c.is_alphanumeric());
    bare.split_whitespace().collect::<Vec<_>>() == ["do", "it", "now"]
}

// ── Chunk timer ───────────────────────────────────────────────────────────────

/// Fires a callback every `CHUNK_INTERVAL_S` seconds until stopped.
struct ChunkTimer {
    stop_tx: std::sync::mpsc::Sender<()>,
}

impl ChunkTimer {
    fn start(callback: impl Fn() + Send + 'static) -> Self {
        let (stop_tx, stop_rx) = channel::<()>();
        thread::spawn(move || loop {
            match stop_rx.recv_timeout(Duration::from_secs(CHUNK_INTERVAL_S)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => callback(),
            }
        });
        ChunkTimer { stop_tx }
    }

    fn stop(self) {
        let _ = self.stop_tx.send(());
    }
}

// ── Max-duration timer ────────────────────────────────────────────────────────

/// Fires a callback once after `duration` unless cancelled first.
struct MaxDurationTimer {
    stop_tx: std::sync::mpsc::Sender<()>,
}

impl MaxDurationTimer {
    fn start(duration: Duration, callback: impl Fn() + Send + 'static) -> Self {
        let (stop_tx, stop_rx) = channel::<()>();
        thread::spawn(move || {
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                stop_rx.recv_timeout(duration)
            {
                callback();
            }
        });
        MaxDurationTimer { stop_tx }
    }

    fn cancel(self) {
        let _ = self.stop_tx.send(());
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct Args {
    config: Option<PathBuf>,
    detach: bool,
    no_tray: bool,
}

fn parse_args() -> Args {
    let mut out = Args::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => {
                if let Some(p) = args.next() {
                    out.config = Some(PathBuf::from(p));
                }
            }
            "--detach"  => out.detach = true,
            "--no-tray" => out.no_tray = true,
            "--tray"    => {} // legacy alias: tray is now the default
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => {}
        }
    }
    out
}

fn print_help() {
    println!(
        "GhostScribe Windows client\n\n\
         Usage: ghostscribe-client.exe [--config PATH] [--no-tray] [--detach]\n\n\
         Hold the configured trigger key to record. Release to transcribe.\n\n\
         Options:\n  \
           --config PATH   Use this TOML config file.\n  \
           --no-tray       Run headless (no tray icon), log to stderr.\n  \
           --detach        Re-spawn as a detached background process with no\n                          \
         console attachment, redirecting all logs to\n                          \
         %APPDATA%\\ghostscribe\\ghostscribe.log. Tray mode detaches\n                          \
         automatically; use this with --no-tray when launching from\n                          \
         an IDE terminal or for autostart-on-login shortcuts.\n  \
           -h, --help      Show this help and exit.\n\n\
         Default behaviour: tray mode (system-tray icon, live config reload).\n\
         The process detaches from the console automatically on first launch.\n\n\
         Config search order:\n  \
           1. --config PATH\n  \
           2. <exe directory>\\config.toml\n  \
           3. %APPDATA%\\ghostscribe\\config.toml\n  \
           4. .\\config.toml\n"
    );
}

fn log_file_path() -> Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA")
        .ok_or_else(|| anyhow!("%APPDATA% is not set; cannot pick a log file location"))?;
    let dir = PathBuf::from(appdata).join("ghostscribe");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create log dir {}", dir.display()))?;
    Ok(dir.join("ghostscribe.log"))
}

/// Re-spawn the current executable as a detached background process whose
/// stdio is redirected to a log file, then exit (the caller does the exit).
///
/// `extra_args` are appended after the forwarded args; callers use this to
/// pass `--tray` when promoting a `--tray`-only invocation into a detached
/// tray session. `--detach` is always stripped from the forwarded args so
/// the child does not recurse.
fn spawn_detached(extra_args: &[&str]) -> Result<u32> {
    let exe = std::env::current_exe().context("current_exe")?;

    let log_path = log_file_path()?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let log_dup = log.try_clone().context("cloning log handle for stderr")?;

    let mut forwarded: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--detach" && a != "--tray") // --tray is legacy no-op; --no-tray is kept
        .collect();
    for a in extra_args {
        forwarded.push((*a).to_string());
    }

    let child = Command::new(&exe)
        .args(&forwarded)
        .env(ENV_DETACHED, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_dup))
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("spawning detached {}", exe.display()))?;

    eprintln!("ghostscribe-client detached (pid {})", child.id());
    eprintln!("logs: {}", log_path.display());
    Ok(child.id())
}

fn already_detached() -> bool {
    std::env::var_os(ENV_DETACHED).as_deref() == Some(std::ffi::OsStr::new("1"))
}

fn main() -> Result<()> {
    let args = parse_args();

    // Tray is the default mode. It always detaches so that double-clicking
    // the exe doesn't leave a console window open. We only auto-detach once
    // (ENV_DETACHED on the child breaks the recursion).
    //
    // --no-tray opts into headless mode. --detach with --no-tray spawns a
    // detached headless process (useful for IDE terminals / autostart).
    if !args.no_tray && !already_detached() {
        // Tray mode: detach and run with tray icon.
        spawn_detached(&[])?;
        return Ok(());
    }
    if args.no_tray && args.detach && !already_detached() {
        // Headless + explicit detach: re-spawn without console.
        spawn_detached(&["--no-tray"])?;
        return Ok(());
    }

    let cfg = config::load(args.config.as_deref())?;

    if args.no_tray {
        run_headless(cfg)
    } else {
        run_tray(cfg, args)
    }
}

// -----------------------------------------------------------------------------
// Headless mode (original behaviour)
// -----------------------------------------------------------------------------

fn print_banner(cfg: &ClientConfig, mode: &str) {
    logln!("GhostScribe client ({mode}) -> {}", cfg.url());
    match &cfg.source_path {
        Some(p) => logln!("config:   {}", p.display()),
        None => logln!("config:   (defaults, no config file found)"),
    }
    logln!("trigger:  {}", cfg.trigger);
    logln!(
        "one_key:  {}",
        if cfg.one_key_trigger.is_empty() { "off" } else { cfg.one_key_trigger.as_str() }
    );
    logln!("format:   {}", cfg.audio_format);
    logln!("auth:     {}", if cfg.has_auth() { "on" } else { "off" });
    logln!(
        "paste:    {} (delay {} ms)",
        if cfg.auto_paste { "on" } else { "off" },
        cfg.paste_delay_ms
    );
}

fn run_headless(cfg: ClientConfig) -> Result<()> {
    let trigger = parse_trigger(&cfg.trigger)?;
    let one_key = parse_one_key_trigger(&cfg.one_key_trigger)?;

    print_banner(&cfg, "headless");

    let recorder = Recorder::start(&cfg.input_device)?;
    let last_paste: Arc<std::sync::Mutex<Option<std::time::Instant>>> =
        Arc::new(std::sync::Mutex::new(None));

    logln!(
        "Hold {} and speak. Release to transcribe. Ctrl+C to quit.",
        cfg.trigger
    );

    let (tx, rx) = channel::<HotkeyEvent>();
    thread::spawn(move || {
        if let Err(e) = run_hook(tx, trigger, one_key) {
            eprintln!("[fatal] keyboard hook failed: {e}");
            std::process::exit(1);
        }
    });

    for event in rx {
        match event {
            HotkeyEvent::Press => {
                recorder.begin();
                logln!("[rec] ...");
            }
            HotkeyEvent::Cancel => {
                recorder.cancel();
                logln!("[rec] cancelled");
            }
            HotkeyEvent::Release => match recorder.end() {
                None => logln!("[rec] stopped, no audio captured"),
                Some(samples) => {
                    let raw_kb = samples.len() * 2 / 1024;
                    logln!("[rec] stopped, {raw_kb} kB raw");
                    spawn_upload_headless(&cfg, samples, last_paste.clone());
                }
            },
        }
    }

    Ok(())
}

fn spawn_upload_headless(
    cfg: &ClientConfig,
    samples: Vec<i16>,
    last_paste: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
) {
    let cfg = cfg.clone();
    thread::spawn(move || {
        if let Err(e) = do_upload_and_paste(&cfg, samples, &last_paste) {
            logln!("[send] {e}");
        }
    });
}

/// Upload + paste pipeline, shared between headless and tray modes. Returns
/// the transcript text on success so the caller can surface it (headless
/// prints it to stdout; tray updates the tooltip).
///
/// `last_paste` tracks when the previous paste occurred for smart-space continuation.
fn do_upload_and_paste(
    cfg: &ClientConfig,
    samples: Vec<i16>,
    last_paste: &std::sync::Mutex<Option<std::time::Instant>>,
) -> Result<String> {
    let (encoded, filename, mime) = audio::encode(&samples, &cfg.audio_format)
        .map_err(|e| anyhow!("encoding failed: {e}"))?;
    logln!("[send] {} kB {}", encoded.len() / 1024, cfg.audio_format);

    let t = upload::submit(cfg, &encoded, filename, mime)?;
    let kb = t.bytes_sent / 1024;
    logln!(
        "[recv] {} kB in {} ms (lang={} p={:.2})",
        kb, t.elapsed_ms, t.language, t.language_probability
    );
    if t.text.is_empty() {
        logln!("[recv] empty transcript");
        return Ok(String::new());
    }
    logln!("[recv] transcript:");
    println!("{}", t.text);

    if cfg.auto_paste {
        if is_do_it_now(&t.text) {
            logln!("[do-it-now] Enter");
            paste::inject_enter();
            return Ok(t.text);
        }

        let mut lp = last_paste.lock().unwrap();
        let needs_space = cfg.smart_space
            && lp.map_or(false, |t| {
                t.elapsed().as_secs_f32() < cfg.continuation_window_s as f32
            })
            && t.text.chars().next().map_or(false, |c| !c.is_whitespace());
        let paste_text = if needs_space {
            format!(" {} ", t.text)
        } else {
            format!("{} ", t.text)
        };
        let saved = paste::get_clipboard();
        match paste::set_clipboard(&paste_text) {
            Err(e) => logln!("[paste] clipboard write failed: {e}"),
            Ok(()) => {
                paste::inject_ctrl_v(cfg.paste_delay_ms);
                *lp = Some(std::time::Instant::now());
                // Wait for the target window to read the clipboard before
                // restoring the saved value. paste_delay_ms (default 50 ms)
                // is too short under load; enforce a 150 ms floor. The delay
                // is invisible — the pasted text has already appeared.
                let restore_ms = cfg.paste_delay_ms.max(150);
                thread::sleep(Duration::from_millis(restore_ms as u64));
                if let Some(prev) = saved {
                    let _ = paste::set_clipboard(&prev);
                }
                logln!("[paste] done");
            }
        }
    }

    Ok(t.text)
}

// -----------------------------------------------------------------------------
// Tray mode
// -----------------------------------------------------------------------------

use ghostscribe_client::tray::{self, MenuAction, Tray, TrayState};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};

#[derive(Debug)]
enum UserEvent {
    Hotkey(HotkeyEvent),
    Menu(MenuAction),
    Watcher(WatcherEvent),
    UploadOk(String),
    UploadErr(String),
    /// A periodic chunk upload completed while recording continued.
    ChunkUploadOk(String),
    /// Auto-chunk timer fired: checkpoint and upload partial audio.
    ChunkFired,
    /// Max-duration timer fired: force-stop the recording.
    MaxDurationFired,
}

fn run_tray(initial: ClientConfig, args: Args) -> Result<()> {
    print_banner(&initial, "tray");

    let trigger = parse_trigger(&initial.trigger)?;
    let one_key = parse_one_key_trigger(&initial.one_key_trigger)?;

    let cfg_store = Arc::new(ArcSwap::from_pointee(initial.clone()));

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Hotkey hook → adapter thread → proxy.
    {
        let (hk_tx, hk_rx) = channel::<HotkeyEvent>();
        thread::spawn(move || {
            if let Err(e) = run_hook(hk_tx, trigger, one_key) {
                eprintln!("[fatal] keyboard hook failed: {e}");
            }
        });
        let proxy = proxy.clone();
        thread::spawn(move || {
            while let Ok(ev) = hk_rx.recv() {
                if proxy.send_event(UserEvent::Hotkey(ev)).is_err() {
                    return;
                }
            }
        });
    }

    // Menu events → proxy.
    {
        let proxy = proxy.clone();
        thread::spawn(move || {
            let rx = tray_icon::menu::MenuEvent::receiver();
            while let Ok(ev) = rx.recv() {
                if let Some(a) = MenuAction::from_id(ev.id.as_ref()) {
                    if proxy.send_event(UserEvent::Menu(a)).is_err() {
                        return;
                    }
                }
            }
        });
    }

    // Config watcher → proxy. Only meaningful if the user has a file on
    // disk; pure-default sessions have nothing to watch.
    if let Some(path) = initial.source_path.clone() {
        let store = cfg_store.clone();
        let proxy = proxy.clone();
        watcher::spawn(
            path,
            move || (**store.load()).clone(),
            move |ev: WatcherEvent| proxy.send_event(UserEvent::Watcher(ev)).map_err(|_| ()),
        );
    }

    let recorder = Recorder::start(&initial.input_device)?;
    let mut tray: Option<Tray> = None;
    let mut pending_restart: Vec<&'static str> = Vec::new();
    let recorder_opt = Some(recorder);
    let cfg_path_for_menu = initial.source_path.clone();
    let args_for_restart = args.clone();
    let last_paste: Arc<std::sync::Mutex<Option<std::time::Instant>>> =
        Arc::new(std::sync::Mutex::new(None));

    let mut is_recording = false;
    let mut chunk_timer: Option<ChunkTimer> = None;
    let mut max_timer: Option<MaxDurationTimer> = None;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        // Create the tray icon once the Win32 event loop is running.
        if let Event::NewEvents(StartCause::Init) = event {
            tray = Some(
                Tray::new(cfg_path_for_menu.clone())
                    .expect("tray icon init failed"),
            );
            return;
        }

        let Some(mut tray) = tray.as_mut() else { return };

        match event {
            Event::UserEvent(UserEvent::Hotkey(HotkeyEvent::Press)) => {
                if let Some(r) = recorder_opt.as_ref() {
                    r.begin();
                    is_recording = true;
                    let _ = tray.set_state(TrayState::Recording);

                    // Auto-chunk timer.
                    let p = proxy.clone();
                    chunk_timer = Some(ChunkTimer::start(move || {
                        let _ = p.send_event(UserEvent::ChunkFired);
                    }));

                    // Max-duration timer (0 = disabled).
                    let cfg_snap = cfg_store.load_full();
                    if cfg_snap.max_record_s > 0 {
                        let p = proxy.clone();
                        let dur = Duration::from_secs(cfg_snap.max_record_s as u64);
                        max_timer = Some(MaxDurationTimer::start(dur, move || {
                            let _ = p.send_event(UserEvent::MaxDurationFired);
                        }));
                    }
                }
            }
            Event::UserEvent(UserEvent::Hotkey(HotkeyEvent::Cancel)) => {
                if let Some(t) = chunk_timer.take() { t.stop(); }
                if let Some(t) = max_timer.take() { t.cancel(); }
                if let Some(r) = recorder_opt.as_ref() {
                    r.cancel();
                }
                is_recording = false;
                let _ = tray.set_state(TrayState::Idle);
            }
            Event::UserEvent(UserEvent::Hotkey(HotkeyEvent::Release))
            | Event::UserEvent(UserEvent::MaxDurationFired) => {
                if let Some(t) = chunk_timer.take() { t.stop(); }
                if let Some(t) = max_timer.take() { t.cancel(); }
                is_recording = false;
                let samples = recorder_opt.as_ref().and_then(|r| r.end());
                match samples {
                    None => {
                        let _ = tray.set_state(TrayState::Idle);
                    }
                    Some(samples) => {
                        let _ = tray.set_state(TrayState::Uploading);
                        let cfg_snap = cfg_store.load_full();
                        let proxy = proxy.clone();
                        let lp = last_paste.clone();
                        thread::spawn(move || {
                            match do_upload_and_paste(&cfg_snap, samples, &lp) {
                                Ok(text)  => { let _ = proxy.send_event(UserEvent::UploadOk(text)); }
                                Err(e)    => { let _ = proxy.send_event(UserEvent::UploadErr(format!("{e:#}"))); }
                            }
                        });
                    }
                }
            }
            Event::UserEvent(UserEvent::ChunkFired) => {
                if let Some(samples) = recorder_opt.as_ref().and_then(|r| r.checkpoint()) {
                    let cfg_snap = cfg_store.load_full();
                    let proxy = proxy.clone();
                    let lp = last_paste.clone();
                    thread::spawn(move || {
                        match do_upload_and_paste(&cfg_snap, samples, &lp) {
                            Ok(text)  => { let _ = proxy.send_event(UserEvent::ChunkUploadOk(text)); }
                            Err(e)    => { let _ = proxy.send_event(UserEvent::UploadErr(format!("{e:#}"))); }
                        }
                    });
                }
            }
            Event::UserEvent(UserEvent::UploadOk(text)) => {
                let _ = tray.set_state(TrayState::Idle);
                if !text.is_empty() {
                    tray.set_tooltip_suffix(&format!("last: {} chars", text.chars().count()));
                }
            }
            Event::UserEvent(UserEvent::ChunkUploadOk(text)) => {
                // Restore recording state if still active, idle otherwise.
                let next = if is_recording { TrayState::Recording } else { TrayState::Idle };
                let _ = tray.set_state(next);
                if !text.is_empty() {
                    tray.set_tooltip_suffix(&format!("chunk: {} chars", text.chars().count()));
                }
            }
            Event::UserEvent(UserEvent::UploadErr(msg)) => {
                logln!("[send] {msg}");
                let _ = tray.set_state(TrayState::Error);
                tray.set_tooltip_suffix(&truncate(&msg, 80));
            }
            Event::UserEvent(UserEvent::Watcher(w)) => {
                handle_watcher_event(w, &cfg_store, &mut tray, &mut pending_restart);
            }
            Event::UserEvent(UserEvent::Menu(a)) => {
                if handle_menu_action(
                    a,
                    &cfg_path_for_menu,
                    &cfg_store,
                    &mut tray,
                    &mut pending_restart,
                    &args_for_restart,
                ) {
                    *control_flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn handle_watcher_event(
    ev: WatcherEvent,
    cfg_store: &Arc<ArcSwap<ClientConfig>>,
    tray: &mut Tray,
    pending_restart: &mut Vec<&'static str>,
) {
    match ev {
        WatcherEvent::Reloaded { new_config, diff } => {
            cfg_store.store(Arc::new(*new_config));
            if !diff.hot_changed.is_empty() {
                let msg = format!("reloaded: {}", diff.hot_changed.join(", "));
                logln!("[config] {msg}");
                tray.set_tooltip_suffix(&msg);
            }
            if !diff.cold_changed.is_empty() {
                for k in &diff.cold_changed {
                    if !pending_restart.contains(k) {
                        pending_restart.push(*k);
                    }
                }
                logln!("[config] restart required: {}", pending_restart.join(", "));
                let _ = tray.set_state(TrayState::Error);
                tray.set_tooltip_suffix(&format!(
                    "restart required: {}",
                    pending_restart.join(", ")
                ));
            }
        }
        WatcherEvent::ParseError { message } => {
            logln!("[config] parse error: {message}");
            let _ = tray.set_state(TrayState::Error);
            tray.set_tooltip_suffix("config parse error — see dialog");
            tray::show_error_box("GhostScribe — config parse error", &message);
        }
        WatcherEvent::Missing => {
            logln!("[config] source file disappeared");
            tray.set_tooltip_suffix("config file missing");
        }
    }
}

/// Returns `true` to request event-loop exit (i.e. Quit).
fn handle_menu_action(
    action: MenuAction,
    cfg_path: &Option<PathBuf>,
    cfg_store: &Arc<ArcSwap<ClientConfig>>,
    tray: &mut Tray,
    pending_restart: &mut Vec<&'static str>,
    args: &Args,
) -> bool {
    match action {
        MenuAction::EditConfig => {
            match ensure_config_file(cfg_path) {
                Ok(p)  => shell_open(&p),
                Err(e) => tray::show_error_box("GhostScribe", &format!("Cannot open config: {e:#}")),
            }
            false
        }
        MenuAction::RevealConfig => {
            if let Some(p) = cfg_path {
                shell_reveal(p);
            }
            false
        }
        MenuAction::ReloadConfig => {
            if let Some(p) = cfg_path {
                match config::load_from(p) {
                    Ok(new_cfg) => {
                        let live = cfg_store.load_full();
                        let d = config::diff(&live, &new_cfg);
                        cfg_store.store(Arc::new(new_cfg));
                        if d.is_empty() {
                            tray.set_tooltip_suffix("reload: no changes");
                        } else if d.cold_changed.is_empty() {
                            tray.set_tooltip_suffix(&format!(
                                "reloaded: {}",
                                d.hot_changed.join(", ")
                            ));
                        } else {
                            for k in &d.cold_changed {
                                if !pending_restart.contains(k) {
                                    pending_restart.push(*k);
                                }
                            }
                            let _ = tray.set_state(TrayState::Error);
                            tray.set_tooltip_suffix(&format!(
                                "restart required: {}",
                                pending_restart.join(", ")
                            ));
                        }
                    }
                    Err(e) => tray::show_error_box(
                        "GhostScribe — config parse error",
                        &format!("{e:#}"),
                    ),
                }
            }
            false
        }
        MenuAction::ShowLog => {
            if let Ok(p) = log_file_path() {
                shell_open(&p);
            }
            false
        }
        MenuAction::ToggleLog => {
            // muda auto-toggles the check state on click; sync our flag.
            LOGGING_ENABLED.store(tray.is_logging_enabled(), Ordering::Relaxed);
            false
        }
        MenuAction::Restart => {
            // In tray mode (the default) we re-spawn with no extra flags;
            // the child will default to tray again.
            let extra: Vec<&str> = if args.no_tray { vec!["--no-tray"] } else { vec![] };
            if let Err(e) = spawn_detached(&extra) {
                tray::show_error_box("GhostScribe", &format!("Restart failed: {e:#}"));
                return false;
            }
            true
        }
        MenuAction::About => {
            let cfg_line = cfg_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(defaults, no file)".to_string());
            let body = format!(
                "GhostScribe Windows client v{}\n\n\
                 config: {}\n\
                 server: {}\n",
                env!("CARGO_PKG_VERSION"),
                cfg_line,
                cfg_store.load().url(),
            );
            show_info_box("About GhostScribe", &body);
            false
        }
        MenuAction::Quit => true,
    }
}

/// Ensure the config file exists at the active path. If the user picked a
/// file but it doesn't exist yet, seed it with [`config::DEFAULT_CONFIG_TOML`]
/// so their editor opens to something useful instead of an empty scratchpad.
fn ensure_config_file(cfg_path: &Option<PathBuf>) -> Result<PathBuf> {
    let path = cfg_path
        .clone()
        .or_else(|| {
            std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("ghostscribe").join("config.toml"))
        })
        .ok_or_else(|| anyhow!("no config path known and %APPDATA% is not set"))?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
        }
        fs::write(&path, config::DEFAULT_CONFIG_TOML)
            .with_context(|| format!("seeding {}", path.display()))?;
    }
    Ok(path)
}

fn shell_open(path: &Path) {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let path_w: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn shell_reveal(path: &Path) {
    // `explorer.exe /select,"path"` opens the parent folder with the file
    // highlighted. We route through CreateProcess so the command line can
    // contain spaces without quoting ceremony.
    let arg = format!("/select,{}", path.display());
    let _ = Command::new("explorer.exe").arg(arg).spawn();
}

fn show_info_box(title: &str, body: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    let title_w: Vec<u16> = title.encode_utf16().chain([0]).collect();
    let body_w: Vec<u16> = body.encode_utf16().chain([0]).collect();
    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR(body_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

use std::os::windows::ffi::OsStrExt;
