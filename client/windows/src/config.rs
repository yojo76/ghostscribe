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
    let mut cfg = defaults();

    for path in candidate_paths(explicit) {
        if path.is_file() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            apply_raw(&mut cfg, toml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?);
            cfg.source_path = Some(path);
            break;
        }
    }

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
