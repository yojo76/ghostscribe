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
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server_url: String,
    pub endpoint: String,
    pub auth_token: String,
    pub input_device: String,
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
            let raw: RawConfig = toml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            if let Some(v) = raw.server_url {
                cfg.server_url = v;
            }
            if let Some(v) = raw.endpoint {
                cfg.endpoint = v;
            }
            if let Some(v) = raw.auth_token {
                cfg.auth_token = v;
            }
            if let Some(v) = raw.input_device {
                cfg.input_device = v;
            }
            cfg.source_path = Some(path);
            break;
        }
    }

    Ok(cfg)
}
