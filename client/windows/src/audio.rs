use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use std::io::Cursor;
use std::sync::{Arc, Mutex};

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

struct Shared {
    buffer: Vec<i16>,
    active: bool,
    src_sample_rate: u32,
    src_channels: u16,
}

pub struct Recorder {
    _stream: Stream,
    shared: Arc<Mutex<Shared>>,
}

fn pick_device(name_filter: &str) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if name_filter.is_empty() {
        return host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device"));
    }
    let needle = name_filter.to_lowercase();
    for device in host.input_devices()? {
        let name = device.name().unwrap_or_default();
        if name.to_lowercase().contains(&needle) {
            return Ok(device);
        }
    }
    Err(anyhow!(
        "no input device matched {:?} (use empty string for default)",
        name_filter
    ))
}

impl Recorder {
    pub fn start(input_device: &str) -> Result<Self> {
        let device = pick_device(input_device).context("selecting input device")?;
        let supported = device
            .default_input_config()
            .context("default input config")?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let src_sample_rate = config.sample_rate.0;
        let src_channels = config.channels;

        let shared = Arc::new(Mutex::new(Shared {
            buffer: Vec::new(),
            active: false,
            src_sample_rate,
            src_channels,
        }));
        let shared_cb = Arc::clone(&shared);

        let err_fn = |e| eprintln!("[audio] stream error: {e}");

        let stream = match sample_format {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let mut g = shared_cb.lock().unwrap();
                    if !g.active {
                        return;
                    }
                    for &s in data {
                        let clamped = s.clamp(-1.0, 1.0);
                        g.buffer.push((clamped * i16::MAX as f32) as i16);
                    }
                },
                err_fn,
                None,
            )?,
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let mut g = shared_cb.lock().unwrap();
                    if !g.active {
                        return;
                    }
                    g.buffer.extend_from_slice(data);
                },
                err_fn,
                None,
            )?,
            SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    let mut g = shared_cb.lock().unwrap();
                    if !g.active {
                        return;
                    }
                    for &s in data {
                        g.buffer.push((s as i32 - 32_768) as i16);
                    }
                },
                err_fn,
                None,
            )?,
            other => return Err(anyhow!("unsupported sample format: {:?}", other)),
        };

        stream.play().context("starting input stream")?;

        eprintln!(
            "device:   {} ({} Hz, {} ch)",
            device.name().unwrap_or_else(|_| "?".into()),
            src_sample_rate,
            src_channels
        );

        Ok(Self {
            _stream: stream,
            shared,
        })
    }

    pub fn begin(&self) {
        let mut g = self.shared.lock().unwrap();
        g.buffer.clear();
        g.active = true;
    }

    pub fn end(&self) -> Option<Vec<i16>> {
        let mut g = self.shared.lock().unwrap();
        g.active = false;
        if g.buffer.is_empty() {
            return None;
        }
        let raw = std::mem::take(&mut g.buffer);
        let mono = downmix(&raw, g.src_channels);
        let resampled = resample_linear(&mono, g.src_sample_rate, TARGET_SAMPLE_RATE);
        Some(resampled)
    }
}

fn downmix(samples: &[i16], channels: u16) -> Vec<i16> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let ch = channels as usize;
    let mut out = Vec::with_capacity(samples.len() / ch);
    for frame in samples.chunks_exact(ch) {
        let sum: i32 = frame.iter().map(|&s| s as i32).sum();
        out.push((sum / ch as i32) as i16);
    }
    out
}

fn resample_linear(samples: &[i16], src_rate: u32, dst_rate: u32) -> Vec<i16> {
    if src_rate == dst_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let src_len = samples.len();
    let dst_len = (src_len as u64 * dst_rate as u64 / src_rate as u64) as usize;
    let mut out = Vec::with_capacity(dst_len);
    for i in 0..dst_len {
        let pos = i as f64 * src_rate as f64 / dst_rate as f64;
        let idx = pos.floor() as usize;
        let frac = pos - idx as f64;
        let a = samples[idx.min(src_len - 1)] as f64;
        let b = samples[(idx + 1).min(src_len - 1)] as f64;
        out.push((a + (b - a) * frac) as i16);
    }
    out
}

pub fn encode_wav(samples: &[i16]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for &s in samples {
            writer.write_sample(s)?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}

pub fn encode_flac(samples: &[i16]) -> Result<Vec<u8>> {
    use flacenc::bitsink::MemSink;
    use flacenc::component::BitRepr;
    use flacenc::config::Encoder as FlacConfig;
    use flacenc::encode::encode_with_fixed_block_size;

    let samples_i32: Vec<i32> = samples.iter().map(|&s| s as i32).collect();
    let stream = encode_with_fixed_block_size(
        &FlacConfig::default(),
        samples_i32,
        4096,
        1,
        16,
        TARGET_SAMPLE_RATE as usize,
    )
    .map_err(|e| anyhow!("FLAC encode error: {e:?}"))?;

    let mut sink = MemSink::<u8>::new();
    stream
        .write(&mut sink)
        .map_err(|e| anyhow!("FLAC write error: {e:?}"))?;
    Ok(sink.into_inner())
}

/// Returns (encoded_bytes, filename, mime_type).
pub fn encode(samples: &[i16], format: &str) -> Result<(Vec<u8>, &'static str, &'static str)> {
    match format {
        "flac" => encode_flac(samples).map(|b| (b, "recording.flac", "audio/flac")),
        "wav"  => encode_wav(samples).map(|b| (b, "recording.wav", "audio/wav")),
        other  => Err(anyhow!("unknown audio_format {other:?}; use 'flac' or 'wav'")),
    }
}
