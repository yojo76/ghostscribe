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
    request_timeout_s: Option<u64>,
    smart_space: Option<bool>,
    continuation_window_s: Option<u32>,
    max_record_s: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server_url: String,
    pub endpoint: String,
    pub auth_token: String,
    pub input_device: String,
    pub trigger: String,
    /// Optional single-key PTT. Empty = disabled. Allowed forms:
    /// `key:ctrl`, `key:alt`, `key:f1`..`key:f12`.
    pub one_key_trigger: String,
    pub audio_format: String,
    pub auto_paste: bool,
    pub paste_delay_ms: u32,
    pub request_timeout_s: u64,
    pub smart_space: bool,
    pub continuation_window_s: u32,
    pub max_record_s: u32,
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
/// "Edit config…" and no file exists yet.
pub const DEFAULT_CONFIG_TOML: &str = r#"# GhostScribe Linux client config
# All keys are optional; commented lines show the built-in defaults.

# server_url           = "http://localhost:5005"
# endpoint             = "/v1/auto"
# auth_token           = ""
# input_device         = ""                 # substring match against device name
# trigger              = "key:ctrl+g"       # push-to-talk chord
# one_key_trigger      = ""                 # optional single-key PTT, e.g. key:f11
# audio_format         = "flac"             # "flac" or "wav"
# auto_paste           = true
# paste_delay_ms       = 50
# request_timeout_s    = 30                 # HTTP POST timeout in seconds
# smart_space          = true               # prepend space when continuing dictation
# continuation_window_s = 30               # seconds after last paste that counts as continuation
# max_record_s         = 300               # auto-stop recording after this many seconds (0 = off)
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
        request_timeout_s: 30,
        smart_space: true,
        continuation_window_s: 30,
        max_record_s: 300,
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

    // XDG_CONFIG_HOME, falling back to ~/.config
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(xdg).join("ghostscribe").join("config.toml"));
    } else if let Some(home) = std::env::var_os("HOME") {
        paths.push(
            PathBuf::from(home)
                .join(".config")
                .join("ghostscribe")
                .join("config.toml"),
        );
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
    if let Some(v) = raw.server_url          { cfg.server_url = v; }
    if let Some(v) = raw.endpoint            { cfg.endpoint = v; }
    if let Some(v) = raw.auth_token          { cfg.auth_token = v; }
    if let Some(v) = raw.input_device        { cfg.input_device = v; }
    if let Some(v) = raw.trigger             { cfg.trigger = v; }
    if let Some(v) = raw.one_key_trigger     { cfg.one_key_trigger = v; }
    if let Some(v) = raw.audio_format        { cfg.audio_format = v; }
    if let Some(v) = raw.auto_paste          { cfg.auto_paste = v; }
    if let Some(v) = raw.paste_delay_ms      { cfg.paste_delay_ms = v; }
    if let Some(v) = raw.request_timeout_s   { cfg.request_timeout_s = v; }
    if let Some(v) = raw.smart_space         { cfg.smart_space = v; }
    if let Some(v) = raw.continuation_window_s { cfg.continuation_window_s = v; }
    if let Some(v) = raw.max_record_s        { cfg.max_record_s = v; }
}

pub const HOT_KEYS: &[&str] = &[
    "server_url",
    "endpoint",
    "auth_token",
    "auto_paste",
    "paste_delay_ms",
    "request_timeout_s",
    "smart_space",
    "continuation_window_s",
    "max_record_s",
];

pub const COLD_KEYS: &[&str] = &[
    "trigger",
    "one_key_trigger",
    "input_device",
    "audio_format",
];

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

pub fn diff(old: &ClientConfig, new: &ClientConfig) -> ConfigDiff {
    let mut d = ConfigDiff::default();
    if old.server_url           != new.server_url           { d.hot_changed.push("server_url"); }
    if old.endpoint             != new.endpoint             { d.hot_changed.push("endpoint"); }
    if old.auth_token           != new.auth_token           { d.hot_changed.push("auth_token"); }
    if old.auto_paste           != new.auto_paste           { d.hot_changed.push("auto_paste"); }
    if old.paste_delay_ms       != new.paste_delay_ms       { d.hot_changed.push("paste_delay_ms"); }
    if old.request_timeout_s    != new.request_timeout_s    { d.hot_changed.push("request_timeout_s"); }
    if old.smart_space          != new.smart_space          { d.hot_changed.push("smart_space"); }
    if old.continuation_window_s != new.continuation_window_s { d.hot_changed.push("continuation_window_s"); }
    if old.max_record_s         != new.max_record_s         { d.hot_changed.push("max_record_s"); }

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
    fn url_trims_trailing_server_slash() {
        let cfg = ClientConfig {
            server_url: "http://h:5005/".into(),
            endpoint: "/v1/en".into(),
            ..defaults()
        };
        assert_eq!(cfg.url(), "http://h:5005/v1/en");
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
        assert_eq!(d.request_timeout_s, 30);
        assert!(d.smart_space);
        assert_eq!(d.continuation_window_s, 30);
        assert_eq!(d.max_record_s, 300);
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
            request_timeout_s = 60
            smart_space = false
            continuation_window_s = 10
            max_record_s = 120
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
        assert_eq!(cfg.request_timeout_s, 60);
        assert!(!cfg.smart_space);
        assert_eq!(cfg.continuation_window_s, 10);
        assert_eq!(cfg.max_record_s, 120);
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
