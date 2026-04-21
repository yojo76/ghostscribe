//! GhostScribe Windows push-to-talk client.
//!
//! Hold the configured trigger (default Ctrl+G) to record from the microphone.
//! Release either key to encode WAV, POST to the server, and paste the transcript.

mod audio;
mod config;
mod hotkey;
mod paste;
mod upload;

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::audio::Recorder;
use crate::config::ClientConfig;
use crate::hotkey::{parse_trigger, run_hook, HotkeyEvent};

fn parse_args() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => {
                if let Some(p) = args.next() {
                    return Some(PathBuf::from(p));
                }
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => {}
        }
    }
    None
}

fn print_help() {
    println!(
        "GhostScribe Windows client\n\n\
         Usage: ghostscribe-client.exe [--config PATH]\n\n\
         Hold the configured trigger key to record. Release to transcribe.\n\
         Config search order:\n  \
           1. --config PATH\n  \
           2. <exe directory>\\config.toml\n  \
           3. %APPDATA%\\ghostscribe\\config.toml\n  \
           4. .\\config.toml\n"
    );
}

fn main() -> Result<()> {
    let explicit = parse_args();
    let cfg = config::load(explicit.as_deref())?;

    let trigger = parse_trigger(&cfg.trigger)?;

    eprintln!("GhostScribe client -> {}", cfg.url());
    match &cfg.source_path {
        Some(p) => eprintln!("config:   {}", p.display()),
        None => eprintln!("config:   (defaults, no config file found)"),
    }
    eprintln!("trigger:  {}", cfg.trigger);
    eprintln!("format:   {}", cfg.audio_format);
    eprintln!("auth:     {}", if cfg.has_auth() { "on" } else { "off" });
    eprintln!("paste:    {} (delay {} ms)", if cfg.auto_paste { "on" } else { "off" }, cfg.paste_delay_ms);

    let recorder = Recorder::start(&cfg.input_device)?;

    eprintln!("Hold {} and speak. Release to transcribe. Ctrl+C to quit.", cfg.trigger);

    let (tx, rx) = channel::<HotkeyEvent>();
    thread::spawn(move || {
        if let Err(e) = run_hook(tx, trigger) {
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
                    match paste::set_clipboard(&t.text) {
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
