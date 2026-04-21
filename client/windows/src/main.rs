//! GhostScribe Windows 11 push-to-talk client.
//!
//! Hold Ctrl + G -> record from default microphone.
//! Release either Ctrl or G -> encode WAV, POST to the server, print transcript.

mod audio;
mod config;
mod hotkey;
mod upload;

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::thread;

use anyhow::Result;

use crate::audio::Recorder;
use crate::config::ClientConfig;
use crate::hotkey::{run_hook, HotkeyEvent};

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
         Hold Ctrl+G to record. Release either key to transcribe.\n\
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

    eprintln!("GhostScribe client -> {}", cfg.url());
    match &cfg.source_path {
        Some(p) => eprintln!("config:   {}", p.display()),
        None => eprintln!("config:   (defaults, no config file found)"),
    }
    eprintln!("trigger:  key:ctrl+g");
    eprintln!("auth:     {}", if cfg.has_auth() { "on" } else { "off" });

    let recorder = Recorder::start(&cfg.input_device)?;

    eprintln!("Hold Ctrl+G and speak. Release to transcribe. Ctrl+C to quit.");

    let (tx, rx) = channel::<HotkeyEvent>();
    thread::spawn(move || {
        if let Err(e) = run_hook(tx) {
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
        let wav = match audio::encode_wav(&samples) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[send] encoding failed: {e}");
                return;
            }
        };
        match upload::submit(&cfg, &wav) {
            Ok(t) => {
                let kb = t.bytes_sent / 1024;
                eprintln!(
                    "[recv] {} kB in {} ms (lang={} p={:.2})",
                    kb, t.elapsed_ms, t.language, t.language_probability
                );
                if t.text.is_empty() {
                    eprintln!("[recv] empty transcript");
                } else {
                    eprintln!("[recv] transcript:");
                    println!("{}", t.text);
                }
            }
            Err(e) => eprintln!("[send] {e}"),
        }
    });
}
