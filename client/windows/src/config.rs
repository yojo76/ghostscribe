use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct RawConfig {
    server_url: Option<String>,
    endpoint: Option<String>,
    auth_token: Option<String>,
    input_device: Option<String>,
    trigger: Option<String>,
    one_key_trigger: Option<String>,
    audio_format: Option<String>,
    auto_paste: Option<bool>,
    paste_delay_ms: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server_url: String,
    pub endpoint: String,
    pub auth_token: String,
    pub input_device: String,
    pub trigger: String,
    /// Optional single-key PTT. Empty = disabled. Allowed forms:
    /// `key:ctrl`, `key:alt`, `key:f1`..`key:f24`.
    pub one_key_trigger: String,
    pub audio_format: String,
    pub auto_paste: bool,
    pub paste_delay_ms: u32,
    pub source_path: Option<PathBuf>,
}

impl ClientConfig {
    pub fn url(&self) -> String {
        format!(
            "{}/{}",
            self.server_url.trim_end_matches('/'),
            self.endpoint.trim_start_matches('/')
        )
    }

    pub fn has_auth(&self) -> bool {
        !self.auth_token.is_empty()
    }
}

/// Text seeded into a freshly-created `config.toml` when the user clicks
/// "Edit config…" and no file exists yet. Kept in lock-step with the real
/// defaults in `defaults()` below; the commented defaults double as docs.
pub const DEFAULT_CONFIG_TOML: &str = r#"# GhostScribe Windows client config
# All keys are optional; commented lines show the built-in defaults.

# server_url     = "http://localhost:5005"
# endpoint       = "/v1/auto"
# auth_token     = ""
# input_device   = ""                 # substring match against device name
# trigger        = "key:ctrl+g"       # push-to-talk chord
# one_key_trigger = ""                 # optional single-key PTT, e.g. key:f11
# audio_format   = "flac"             # "flac" or "wav"
# auto_paste     = true
# paste_delay_ms = 50
"#;

fn defaults() -> ClientConfig {
    ClientConfig {
        server_url: "http://localhost:5005".to_string(),
        endpoint: "/v1/auto".to_string(),
        auth_token: String::new(),
        input_device: String::new(),
        trigger: "key:ctrl+g".to_string(),
        one_key_trigger: String::new(),
        audio_format: "flac".to_string(),
        auto_paste: true,
        paste_delay_ms: 50,
        source_path: None,
    }
}

fn candidate_paths(explicit: Option<&Path>) -> Vec<PathBuf> {
    if let Some(p) = explicit {
        return vec![p.to_path_buf()];
    }
    let mut paths = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            paths.push(parent.join("config.toml"));
        }
    }

    if let Ok(appdata) = std::env::var("APPDATA") {
        paths.push(PathBuf::from(appdata).join("ghostscribe").join("config.toml"));
    }

    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join("config.toml"));
    }

    paths
}

pub fn load(explicit: Option<&Path>) -> Result<ClientConfig> {
    for path in candidate_paths(explicit) {
        if path.is_file() {
            return load_from(&path);
        }
    }
    Ok(defaults())
}

/// Parse a specific file on disk into a fresh `ClientConfig`. Used by the
/// watcher to re-validate a config after the user saves without re-running
/// the candidate-path search (which could pick a different file if the
/// user deletes the active one).
pub fn load_from(path: &Path) -> Result<ClientConfig> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let raw: RawConfig = toml::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    let mut cfg = defaults();
    apply_raw(&mut cfg, raw);
    cfg.source_path = Some(path.to_path_buf());
    Ok(cfg)
}

fn apply_raw(cfg: &mut ClientConfig, raw: RawConfig) {
    if let Some(v) = raw.server_url      { cfg.server_url = v; }
    if let Some(v) = raw.endpoint        { cfg.endpoint = v; }
    if let Some(v) = raw.auth_token      { cfg.auth_token = v; }
    if let Some(v) = raw.input_device    { cfg.input_device = v; }
    if let Some(v) = raw.trigger         { cfg.trigger = v; }
    if let Some(v) = raw.one_key_trigger { cfg.one_key_trigger = v; }
    if let Some(v) = raw.audio_format    { cfg.audio_format = v; }
    if let Some(v) = raw.auto_paste      { cfg.auto_paste = v; }
    if let Some(v) = raw.paste_delay_ms  { cfg.paste_delay_ms = v; }
}

/// Keys that can be swapped into a running client without a restart.
/// Readers re-snapshot the config on every upload/paste, so atomic
/// replacement is enough.
pub const HOT_KEYS: &[&str] = &[
    "server_url",
    "endpoint",
    "auth_token",
    "auto_paste",
    "paste_delay_ms",
];

/// Keys whose change requires re-registering the keyboard hook, rebuilding
/// the audio stream, or otherwise reinitialising long-lived resources that
/// were captured at startup. The tray shows a "Restart required" balloon
/// when any of these diverge and enables the "Restart client" menu item.
pub const COLD_KEYS: &[&str] = &[
    "trigger",
    "one_key_trigger",
    "input_device",
    "audio_format",
];

/// Outcome of comparing a freshly-loaded config against the live one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConfigDiff {
    pub hot_changed: Vec<&'static str>,
    pub cold_changed: Vec<&'static str>,
}

impl ConfigDiff {
    pub fn is_empty(&self) -> bool {
        self.hot_changed.is_empty() && self.cold_changed.is_empty()
    }

    pub fn requires_restart(&self) -> bool {
        !self.cold_changed.is_empty()
    }
}

/// Classify which keys changed between the live config and a newly-parsed one.
/// `source_path` is intentionally ignored: rewriting the same file doesn't
/// count as a "config change" for reload purposes.
pub fn diff(old: &ClientConfig, new: &ClientConfig) -> ConfigDiff {
    let mut d = ConfigDiff::default();
    if old.server_url     != new.server_url     { d.hot_changed.push("server_url"); }
    if old.endpoint       != new.endpoint       { d.hot_changed.push("endpoint"); }
    if old.auth_token     != new.auth_token     { d.hot_changed.push("auth_token"); }
    if old.auto_paste     != new.auto_paste     { d.hot_changed.push("auto_paste"); }
    if old.paste_delay_ms != new.paste_delay_ms { d.hot_changed.push("paste_delay_ms"); }

    if old.trigger         != new.trigger         { d.cold_changed.push("trigger"); }
    if old.one_key_trigger != new.one_key_trigger { d.cold_changed.push("one_key_trigger"); }
    if old.input_device    != new.input_device    { d.cold_changed.push("input_device"); }
    if old.audio_format    != new.audio_format    { d.cold_changed.push("audio_format"); }

    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_trims_trailing_server_slash() {
        let cfg = ClientConfig {
            server_url: "http://h:5005/".into(),
            endpoint: "/v1/en".into(),
            ..defaults()
        };
        assert_eq!(cfg.url(), "http://h:5005/v1/en");
    }

    #[test]
    fn url_adds_separator_when_endpoint_unslashed() {
        let cfg = ClientConfig {
            server_url: "http://h:5005".into(),
            endpoint: "v1/en".into(),
            ..defaults()
        };
        assert_eq!(cfg.url(), "http://h:5005/v1/en");
    }

    #[test]
    fn has_auth_toggles_on_non_empty_token() {
        let mut cfg = defaults();
        assert!(!cfg.has_auth());
        cfg.auth_token = "s3cret".into();
        assert!(cfg.has_auth());
    }

    #[test]
    fn defaults_match_documented_values() {
        let d = defaults();
        assert_eq!(d.server_url, "http://localhost:5005");
        assert_eq!(d.endpoint, "/v1/auto");
        assert_eq!(d.trigger, "key:ctrl+g");
        assert_eq!(d.audio_format, "flac");
        assert!(d.auto_paste);
        assert_eq!(d.paste_delay_ms, 50);
        assert!(d.auth_token.is_empty());
        assert!(d.input_device.is_empty());
        assert!(d.one_key_trigger.is_empty());
        assert!(d.source_path.is_none());
    }

    #[test]
    fn raw_toml_round_trips_into_clientconfig() {
        let toml_str = r#"
            server_url = "http://example.internal:5005"
            endpoint   = "/v1/en"
            trigger    = "key:ctrl+shift+g"
            one_key_trigger = "key:alt"
            auth_token = "s3cret"
            input_device = "USB Audio"
            audio_format = "wav"
            auto_paste   = false
            paste_delay_ms = 120
        "#;
        let raw: RawConfig = toml::from_str(toml_str).unwrap();
        let mut cfg = defaults();
        apply_raw(&mut cfg, raw);
        assert_eq!(cfg.server_url, "http://example.internal:5005");
        assert_eq!(cfg.endpoint, "/v1/en");
        assert_eq!(cfg.trigger, "key:ctrl+shift+g");
        assert_eq!(cfg.one_key_trigger, "key:alt");
        assert_eq!(cfg.auth_token, "s3cret");
        assert_eq!(cfg.input_device, "USB Audio");
        assert_eq!(cfg.audio_format, "wav");
        assert!(!cfg.auto_paste);
        assert_eq!(cfg.paste_delay_ms, 120);
    }

    #[test]
    fn partial_toml_only_overrides_specified_fields() {
        let toml_str = r#"server_url = "http://alt:9000""#;
        let raw: RawConfig = toml::from_str(toml_str).unwrap();
        let mut cfg = defaults();
        apply_raw(&mut cfg, raw);
        assert_eq!(cfg.server_url, "http://alt:9000");
        assert_eq!(cfg.endpoint, "/v1/auto");
        assert_eq!(cfg.trigger, "key:ctrl+g");
        assert!(cfg.auto_paste);
    }
}
