use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;

/// Public signal route kind used by [`crate::TestServerHelper`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalKind {
    Sawtooth,
    SawtoothDescending,
    Sine { freq_hz: f64 },
    Silence,
}

impl SignalKind {
    #[must_use]
    pub const fn path_segment(self) -> &'static str {
        match self {
            Self::Sawtooth => "sawtooth",
            Self::SawtoothDescending => "sawtooth-desc",
            Self::Sine { .. } => "sine",
            Self::Silence => "silence",
        }
    }
}

/// Output file format for `/signal/...` routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalFormat {
    Wav,
    Mp3,
    Flac,
    Aac,
    M4a,
}

impl SignalFormat {
    #[must_use]
    pub const fn path_ext(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::Aac => "aac",
            Self::M4a => "m4a",
        }
    }
}

/// Length mode for procedural signal generation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalSpecLength {
    Seconds(f64),
    Frames(usize),
    FileBytes(usize),
    Infinite,
}

/// Public request shape for `/signal/...` URL construction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalSpec {
    pub format: SignalFormat,
    pub length: SignalSpecLength,
    pub channels: u16,
    pub sample_rate: u32,
}

#[derive(Debug, Serialize)]
struct SignalPathPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    file_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frames: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    freq: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    infinite: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seconds: Option<f64>,
    channels: u16,
    sample_rate: u32,
}

/// Build a `/signal/...` path from a public spec.
#[must_use]
pub fn signal_path(kind: SignalKind, spec: &SignalSpec) -> String {
    let payload = SignalPathPayload {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        seconds: match spec.length {
            SignalSpecLength::Seconds(seconds) => Some(seconds),
            _ => None,
        },
        frames: match spec.length {
            SignalSpecLength::Frames(frames) => Some(frames),
            _ => None,
        },
        file_bytes: match spec.length {
            SignalSpecLength::FileBytes(file_bytes) => Some(file_bytes),
            _ => None,
        },
        infinite: match spec.length {
            SignalSpecLength::Infinite => Some(true),
            _ => None,
        },
        freq: match kind {
            SignalKind::Sine { freq_hz } => Some(freq_hz),
            SignalKind::Sawtooth | SignalKind::SawtoothDescending | SignalKind::Silence => None,
        },
    };
    let json = serde_json::to_vec(&payload).expect("signal path payload must serialize");
    let spec_b64 = URL_SAFE_NO_PAD.encode(json);
    format!(
        "/signal/{}/{spec_b64}.{}",
        kind.path_segment(),
        spec.format.path_ext()
    )
}
