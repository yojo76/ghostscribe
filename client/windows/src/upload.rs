use anyhow::{anyhow, Result};
use rand::Rng;
use std::io::Read;
use std::time::Duration;

use crate::config::ClientConfig;

pub struct Transcript {
    pub text: String,
    pub language: String,
    pub language_probability: f64,
    pub bytes_sent: usize,
    pub elapsed_ms: u128,
}

fn random_boundary() -> String {
    let mut rng = rand::thread_rng();
    let suffix: String = (0..16)
        .map(|_| {
            let n: u8 = rng.gen_range(0..36);
            if n < 10 {
                (b'0' + n) as char
            } else {
                (b'a' + (n - 10)) as char
            }
        })
        .collect();
    format!("----ghostscribe-{}", suffix)
}

fn build_multipart(boundary: &str, filename: &str, mime: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(payload.len() + 256);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"audio\"; filename=\"{filename}\"\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(payload);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

pub fn submit(cfg: &ClientConfig, audio: &[u8], filename: &str, mime: &str) -> Result<Transcript> {
    let boundary = random_boundary();
    let body = build_multipart(&boundary, filename, mime, audio);
    let bytes_sent = body.len();

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build();

    let mut req = agent
        .post(&cfg.url())
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .set("Content-Length", &bytes_sent.to_string());

    if cfg.has_auth() {
        req = req.set("X-Auth-Token", &cfg.auth_token);
    }

    let t0 = std::time::Instant::now();
    let resp = match req.send_bytes(&body) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let msg = r.into_string().unwrap_or_default();
            return Err(anyhow!("HTTP {code}: {}", msg.trim()));
        }
        Err(e) => return Err(anyhow!("network error: {e}")),
    };
    let elapsed_ms = t0.elapsed().as_millis();

    let mut reader = resp.into_reader();
    let mut buf = String::new();
    reader.read_to_string(&mut buf)?;

    let (text, language, language_probability) = parse_response(&buf)?;

    Ok(Transcript {
        text,
        language,
        language_probability,
        bytes_sent,
        elapsed_ms,
    })
}

fn parse_response(json: &str) -> Result<(String, String, f64)> {
    let text = extract_string(json, "text").unwrap_or_default();
    let language = extract_string(json, "language").unwrap_or_else(|| "?".to_string());
    let prob = extract_number(json, "language_probability").unwrap_or(0.0);
    Ok((text, language, prob))
}

fn extract_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let idx = json.find(&needle)?;
    let tail = &json[idx + needle.len()..];
    let colon = tail.find(':')?;
    let tail = &tail[colon + 1..];
    let tail = tail.trim_start();
    if !tail.starts_with('"') {
        return None;
    }
    let tail = &tail[1..];
    let mut out = String::new();
    let mut chars = tail.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() != 4 {
                        return Some(out);
                    }
                    if let Ok(n) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(n) {
                            out.push(ch);
                        }
                    }
                }
                other => out.push(other),
            },
            '"' => return Some(out),
            _ => out.push(c),
        }
    }
    Some(out)
}

fn extract_number(json: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\"");
    let idx = json.find(&needle)?;
    let tail = &json[idx + needle.len()..];
    let colon = tail.find(':')?;
    let tail = &tail[colon + 1..].trim_start();
    let end = tail
        .find(|c: char| c == ',' || c == '}' || c.is_whitespace())
        .unwrap_or(tail.len());
    tail[..end].parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_boundary_has_expected_shape() {
        let b = random_boundary();
        assert!(b.starts_with("----ghostscribe-"));
        let suffix = &b["----ghostscribe-".len()..];
        assert_eq!(suffix.len(), 16);
        assert!(suffix.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[test]
    fn random_boundary_is_probabilistically_unique() {
        let a = random_boundary();
        let b = random_boundary();
        assert_ne!(a, b);
    }

    #[test]
    fn build_multipart_contains_headers_and_payload() {
        let payload = b"hello-audio";
        let body = build_multipart("xBOUNDARYx", "recording.wav", "audio/wav", payload);
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("--xBOUNDARYx\r\n"));
        assert!(text.contains("Content-Disposition: form-data; name=\"audio\"; filename=\"recording.wav\"\r\n"));
        assert!(text.contains("Content-Type: audio/wav\r\n\r\n"));
        assert!(text.contains("hello-audio"));
        assert!(text.ends_with("\r\n--xBOUNDARYx--\r\n"));
    }

    #[test]
    fn parse_response_extracts_all_fields() {
        let json = r#"{"text":"hello world","language":"en","language_probability":0.97}"#;
        let (text, lang, prob) = parse_response(json).unwrap();
        assert_eq!(text, "hello world");
        assert_eq!(lang, "en");
        assert!((prob - 0.97).abs() < 1e-9);
    }

    #[test]
    fn parse_response_defaults_when_fields_missing() {
        let (text, lang, prob) = parse_response("{}").unwrap();
        assert_eq!(text, "");
        assert_eq!(lang, "?");
        assert_eq!(prob, 0.0);
    }

    #[test]
    fn extract_string_handles_escape_sequences() {
        let json = r#"{"text":"line1\nline2\tend\"quoted\""}"#;
        let text = extract_string(json, "text").unwrap();
        assert_eq!(text, "line1\nline2\tend\"quoted\"");
    }

    #[test]
    fn extract_string_handles_unicode_escape() {
        let json = r#"{"text":"café"}"#;
        assert_eq!(extract_string(json, "text").unwrap(), "café");
    }

    #[test]
    fn extract_string_returns_none_when_key_absent() {
        assert!(extract_string(r#"{"other":"x"}"#, "text").is_none());
    }

    #[test]
    fn extract_number_ignores_trailing_delimiters() {
        assert_eq!(
            extract_number(r#"{"x":0.5,"y":1}"#, "x").unwrap(),
            0.5
        );
        assert_eq!(
            extract_number(r#"{"x":42}"#, "x").unwrap(),
            42.0
        );
        assert_eq!(
            extract_number(r#"{"x": 3.14 }"#, "x").unwrap(),
            3.14
        );
    }

    #[test]
    fn extract_number_returns_none_on_non_numeric() {
        assert!(extract_number(r#"{"x":"hello"}"#, "x").is_none());
    }
}
