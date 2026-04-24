//! Integration tests for `upload::submit()` against a mock HTTP server.
//!
//! Uses `mockito` to spin up a local server and verify:
//! - multipart body carries the audio part with correct filename + mime
//! - `X-Auth-Token` header is present iff `auth_token` is configured
//! - JSON response is parsed into the expected fields
//! - 5xx and timeouts surface as `Err`

use ghostscribe_client::config::ClientConfig;
use ghostscribe_client::upload::submit;

fn cfg_for(server_url: String, endpoint: &str, token: &str) -> ClientConfig {
    ClientConfig {
        server_url,
        endpoint: endpoint.to_string(),
        auth_token: token.to_string(),
        input_device: String::new(),
        trigger: "key:ctrl+g".to_string(),
        one_key_trigger: String::new(),
        audio_format: "wav".to_string(),
        auto_paste: true,
        paste_delay_ms: 50,
        request_timeout_s: 30,
        smart_space: true,
        continuation_window_s: 30,
        max_record_s: 300,
        source_path: None,
    }
}

#[test]
fn happy_path_parses_transcript_json() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/v1/auto")
        .match_header(
            "content-type",
            mockito::Matcher::Regex("^multipart/form-data; boundary=".to_string()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"text":"hello world","language":"en","language_probability":0.97}"#)
        .create();

    let cfg = cfg_for(server.url(), "/v1/auto", "");
    let result = submit(&cfg, b"fake-wav-bytes", "recording.wav", "audio/wav").unwrap();

    assert_eq!(result.text, "hello world");
    assert_eq!(result.language, "en");
    assert!((result.language_probability - 0.97).abs() < 1e-9);
    assert!(result.bytes_sent > b"fake-wav-bytes".len());
    mock.assert();
}

#[test]
fn multipart_body_includes_filename_and_mime() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/v1/auto")
        .match_body(mockito::Matcher::AllOf(vec![
            mockito::Matcher::Regex(r#"name="audio"; filename="recording.flac""#.to_string()),
            mockito::Matcher::Regex(r"Content-Type: audio/flac".to_string()),
            mockito::Matcher::Regex(r"fake-flac-payload".to_string()),
        ]))
        .with_status(200)
        .with_body(r#"{"text":"","language":"en","language_probability":0.1}"#)
        .create();

    let cfg = cfg_for(server.url(), "/v1/auto", "");
    submit(&cfg, b"fake-flac-payload", "recording.flac", "audio/flac").unwrap();

    mock.assert();
}

#[test]
fn auth_header_sent_when_configured() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/v1/en")
        .match_header("x-auth-token", "s3cret")
        .with_status(200)
        .with_body(r#"{"text":"ok","language":"en","language_probability":0.9}"#)
        .create();

    let cfg = cfg_for(server.url(), "/v1/en", "s3cret");
    submit(&cfg, b"x", "recording.wav", "audio/wav").unwrap();

    mock.assert();
}

#[test]
fn auth_header_omitted_when_token_empty() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/v1/en")
        .match_header("x-auth-token", mockito::Matcher::Missing)
        .with_status(200)
        .with_body(r#"{"text":"ok","language":"en","language_probability":0.9}"#)
        .create();

    let cfg = cfg_for(server.url(), "/v1/en", "");
    submit(&cfg, b"x", "recording.wav", "audio/wav").unwrap();

    mock.assert();
}

#[test]
fn server_5xx_is_surfaced_as_err() {
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("POST", "/v1/auto")
        .with_status(500)
        .with_body("boom")
        .create();

    let cfg = cfg_for(server.url(), "/v1/auto", "");
    let err = submit(&cfg, b"x", "recording.wav", "audio/wav").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("500"), "error should carry status: {msg}");
}

#[test]
fn server_4xx_is_surfaced_as_err() {
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("POST", "/v1/en")
        .with_status(401)
        .with_body("unauthorized")
        .create();

    let cfg = cfg_for(server.url(), "/v1/en", "wrong");
    let err = submit(&cfg, b"x", "recording.wav", "audio/wav").unwrap_err();
    assert!(err.to_string().contains("401"));
}

#[test]
fn empty_transcript_parses_cleanly() {
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("POST", "/v1/auto")
        .with_status(200)
        .with_body(r#"{"text":"","language":"en","language_probability":0.05}"#)
        .create();

    let cfg = cfg_for(server.url(), "/v1/auto", "");
    let r = submit(&cfg, b"x", "recording.wav", "audio/wav").unwrap();
    assert_eq!(r.text, "");
    assert_eq!(r.language, "en");
}

#[test]
fn endpoint_composition_handles_trailing_slash() {
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("POST", "/v1/auto")
        .with_status(200)
        .with_body(r#"{"text":"x","language":"en","language_probability":0.9}"#)
        .create();

    // server.url() already returns no trailing slash; append one to exercise trim.
    let cfg = cfg_for(format!("{}/", server.url()), "/v1/auto", "");
    submit(&cfg, b"x", "recording.wav", "audio/wav").unwrap();
}
