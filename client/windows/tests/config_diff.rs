//! Integration coverage for the `ConfigDiff` surface used by the tray
//! watcher. The goal is to pin down which key changes are classified as
//! hot (live-swap) vs cold (restart-required). Regressions here would
//! silently skip reload notifications or — worse — hot-swap something
//! whose change should be gated behind a restart.

use ghostscribe_client::config::{self, ClientConfig};
use std::io::Write;
use std::path::PathBuf;

fn tmp_toml(name: &str, contents: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gs-config-diff-{}", rand_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    f.sync_all().unwrap();
    p
}

fn rand_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
}

fn base() -> ClientConfig {
    // Default load with no file present: produces the built-in defaults.
    config::load(None).unwrap()
}

#[test]
fn hot_keys_report_as_hot_only() {
    let new_path = tmp_toml(
        "hot.toml",
        r#"
server_url = "http://elsewhere:5005"
endpoint   = "/v1/en"
auth_token = "s3cret"
auto_paste = false
paste_delay_ms = 120
"#,
    );
    let new_cfg = config::load_from(&new_path).unwrap();
    let d = config::diff(&base(), &new_cfg);

    for k in ["server_url", "endpoint", "auth_token", "auto_paste", "paste_delay_ms"] {
        assert!(d.hot_changed.contains(&k), "expected {k} in hot_changed");
    }
    assert!(
        d.cold_changed.is_empty(),
        "unexpected cold drift: {:?}",
        d.cold_changed
    );
    assert!(!d.requires_restart());
}

#[test]
fn cold_keys_report_as_cold_and_trigger_restart_flag() {
    let new_path = tmp_toml(
        "cold.toml",
        r#"
trigger         = "key:ctrl+shift+g"
one_key_trigger = "key:f11"
input_device    = "USB Audio"
audio_format    = "wav"
"#,
    );
    let new_cfg = config::load_from(&new_path).unwrap();
    let d = config::diff(&base(), &new_cfg);

    for k in ["trigger", "one_key_trigger", "input_device", "audio_format"] {
        assert!(d.cold_changed.contains(&k), "expected {k} in cold_changed");
    }
    assert!(
        d.hot_changed.is_empty(),
        "unexpected hot drift: {:?}",
        d.hot_changed
    );
    assert!(d.requires_restart());
}

#[test]
fn identical_content_is_a_no_op_diff() {
    // Round-trip: serialize base, reparse, diff. Must be empty.
    let b = base();
    let path = tmp_toml(
        "same.toml",
        &format!(
            r#"
server_url     = {server_url:?}
endpoint       = {endpoint:?}
auth_token     = {auth_token:?}
input_device   = {input_device:?}
trigger        = {trigger:?}
one_key_trigger = {one_key:?}
audio_format   = {audio_format:?}
auto_paste     = {auto_paste}
paste_delay_ms = {paste_delay_ms}
"#,
            server_url = b.server_url,
            endpoint = b.endpoint,
            auth_token = b.auth_token,
            input_device = b.input_device,
            trigger = b.trigger,
            one_key = b.one_key_trigger,
            audio_format = b.audio_format,
            auto_paste = b.auto_paste,
            paste_delay_ms = b.paste_delay_ms,
        ),
    );
    let new_cfg = config::load_from(&path).unwrap();
    let d = config::diff(&b, &new_cfg);
    assert!(d.is_empty(), "expected empty diff, got {d:?}");
}

#[test]
fn source_path_change_alone_is_not_a_diff() {
    // Same semantic values in two different files: diff() ignores
    // source_path so the watcher doesn't spam a reload event when the
    // user saves to a new location (e.g. moved config).
    let toml_body = r#"server_url = "http://x:1""#;
    let a = config::load_from(&tmp_toml("a.toml", toml_body)).unwrap();
    let b = config::load_from(&tmp_toml("b.toml", toml_body)).unwrap();
    assert!(config::diff(&a, &b).is_empty());
}
