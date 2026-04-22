//! GhostScribe Windows push-to-talk client.
//!
//! Hold the configured trigger (default Ctrl+G) to record from the microphone.
//! Release either key to encode WAV, POST to the server, and paste the transcript.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use ghostscribe_client::audio::{self, Recorder};
use ghostscribe_client::config::{self, ClientConfig};
use ghostscribe_client::hotkey::{parse_one_key_trigger, parse_trigger, run_hook, HotkeyEvent};
use ghostscribe_client::{paste, upload};

// Win32 process creation flags. Hand-coded to avoid pulling another `windows`
// crate feature just for two constants.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Default)]
struct Args {
    config: Option<PathBuf>,
    detach: bool,
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
            "--detach" => {
                out.detach = true;
            }
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
         Usage: ghostscribe-client.exe [--config PATH] [--detach]\n\n\
         Hold the configured trigger key to record. Release to transcribe.\n\n\
         Options:\n  \
           --config PATH   Use this TOML config file.\n  \
           --detach        Re-spawn as a detached background process with no\n                          \
         console attachment, redirecting all logs to\n                          \
         %APPDATA%\\ghostscribe\\ghostscribe.log. Use this when\n                          \
         launching from an IDE-integrated terminal (e.g. Cursor)\n                          \
         or for autostart-on-login shortcuts.\n  \
           -h, --help      Show this help and exit.\n\n\
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
/// stdio is redirected to a log file, then exit. The child has no console
/// attachment and no parent/foreground-window relationship with whichever
/// terminal launched us, so synthetic input via `SendInput` reaches all
/// foreground windows uniformly (including Electron/Chromium chat inputs
/// that otherwise reject keystrokes from a co-resident process tree).
fn spawn_detached_and_exit() -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;

    let log_path = log_file_path()?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let log_dup = log.try_clone().context("cloning log handle for stderr")?;

    // Forward every flag *except* --detach so the child runs the normal flow.
    let forwarded: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--detach")
        .collect();

    let child = Command::new(&exe)
        .args(&forwarded)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_dup))
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("spawning detached {}", exe.display()))?;

    eprintln!("ghostscribe-client detached (pid {})", child.id());
    eprintln!("logs: {}", log_path.display());
    Ok(())
}

fn main() -> Result<()> {
    let args = parse_args();

    if args.detach {
        return spawn_detached_and_exit();
    }

    let cfg = config::load(args.config.as_deref())?;

    let trigger = parse_trigger(&cfg.trigger)?;
    let one_key = parse_one_key_trigger(&cfg.one_key_trigger)?;

    eprintln!("GhostScribe client -> {}", cfg.url());
    match &cfg.source_path {
        Some(p) => eprintln!("config:   {}", p.display()),
        None => eprintln!("config:   (defaults, no config file found)"),
    }
    eprintln!("trigger:  {}", cfg.trigger);
    eprintln!(
        "one_key:  {}",
        if cfg.one_key_trigger.is_empty() { "off" } else { cfg.one_key_trigger.as_str() }
    );
    eprintln!("format:   {}", cfg.audio_format);
    eprintln!("auth:     {}", if cfg.has_auth() { "on" } else { "off" });
    eprintln!("paste:    {} (delay {} ms)", if cfg.auto_paste { "on" } else { "off" }, cfg.paste_delay_ms);

    let recorder = Recorder::start(&cfg.input_device)?;

    eprintln!("Hold {} and speak. Release to transcribe. Ctrl+C to quit.", cfg.trigger);

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
                eprintln!("[rec] ...");
            }
            HotkeyEvent::Cancel => {
                recorder.cancel();
                eprintln!("[rec] cancelled");
            }
            HotkeyEvent::Release => {
                match recorder.end() {
                    None => eprintln!("[rec] stopped, no audio captured"),
                    Some(samples) => {
                        let raw_kb = samples.len() * 2 / 1024;
                        eprintln!("[rec] stopped, {raw_kb} kB raw");
                        handle_upload(&cfg, samples);
                    }
                }
            }
        }
    }

    Ok(())
}

fn handle_upload(cfg: &ClientConfig, samples: Vec<i16>) {
    let cfg = cfg.clone();
    thread::spawn(move || {
        let (encoded, filename, mime) = match audio::encode(&samples, &cfg.audio_format) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[send] encoding failed: {e}");
                return;
            }
        };
        eprintln!("[send] {} kB {}", encoded.len() / 1024, cfg.audio_format);
        match upload::submit(&cfg, &encoded, filename, mime) {
            Ok(t) => {
                let kb = t.bytes_sent / 1024;
                eprintln!(
                    "[recv] {} kB in {} ms (lang={} p={:.2})",
                    kb, t.elapsed_ms, t.language, t.language_probability
                );
                if t.text.is_empty() {
                    eprintln!("[recv] empty transcript");
                    return;
                }
                eprintln!("[recv] transcript:");
                println!("{}", t.text);

                if cfg.auto_paste {
                    let saved = paste::get_clipboard();
                    // Trailing space so consecutive takes don't butt up
                    // against each other in the target field.
                    let pasted = format!("{} ", t.text);
                    match paste::set_clipboard(&pasted) {
                        Err(e) => eprintln!("[paste] clipboard write failed: {e}"),
                        Ok(()) => {
                            paste::inject_ctrl_v(cfg.paste_delay_ms);
                            thread::sleep(Duration::from_millis(cfg.paste_delay_ms as u64));
                            if let Some(prev) = saved {
                                let _ = paste::set_clipboard(&prev);
                            }
                            eprintln!("[paste] done");
                        }
                    }
                }
            }
            Err(e) => eprintln!("[send] {e}"),
        }
    });
}
