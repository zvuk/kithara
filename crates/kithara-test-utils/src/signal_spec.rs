use std::mem::size_of;

use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use serde::Deserialize;
use thiserror::Error;

use crate::{consts::Consts, signal_pcm::SignalLength};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalKind {
    Sawtooth,
    SawtoothDescending,
    Sine,
    Silence,
}

impl TryFrom<&str> for SignalKind {
    type Error = SignalRequestError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "sawtooth" => Ok(Self::Sawtooth),
            "sawtooth-desc" => Ok(Self::SawtoothDescending),
            "sine" => Ok(Self::Sine),
            "silence" => Ok(Self::Silence),
            _ => Err(SignalRequestError::InvalidField {
                field: "signal_kind",
                message: "must be one of `sawtooth`, `sawtooth-desc`, `sine`, or `silence`",
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalFormat {
    Wav,
    Mp3,
    Flac,
    Aac,
    M4a,
}

impl SignalFormat {
    pub(crate) const fn path_ext(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::Aac => "aac",
            Self::M4a => "m4a",
        }
    }
}

impl TryFrom<&str> for SignalFormat {
    type Error = SignalRequestError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "wav" => Ok(Self::Wav),
            "mp3" => Ok(Self::Mp3),
            "flac" => Ok(Self::Flac),
            "aac" => Ok(Self::Aac),
            "m4a" => Ok(Self::M4a),
            _ => Err(SignalRequestError::UnsupportedExtension(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SignalRequest {
    pub(crate) format: SignalFormat,
    pub(crate) spec: ResolvedSignalSpec,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolvedSignalSpec {
    pub(crate) kind: SignalKind,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) length: SignalLength,
    pub(crate) sine_freq_hz: Option<f64>,
}

#[derive(Debug, Error)]
pub(crate) enum SignalRequestError {
    #[error("signal format extension must be provided in path or JSON `ext`")]
    MissingExtension,
    #[error("unsupported signal format extension `{0}`")]
    UnsupportedExtension(String),
    #[error("signal spec is too large")]
    SpecTooLarge,
    #[error("signal spec is not valid base64url")]
    InvalidBase64,
    #[error("signal spec is not valid JSON")]
    InvalidJson,
    #[error("invalid signal spec field `{field}`: {message}")]
    InvalidField {
        field: &'static str,
        message: &'static str,
    },
    #[error("signal length cannot be represented as exact PCM frames")]
    InvalidLengthLayout,
    #[error("signal spec exceeds the PCM byte budget")]
    PcmBudgetExceeded,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalSpecPayload {
    sample_rate: u32,
    channels: u16,
    #[serde(default)]
    ext: Option<String>,
    #[serde(default)]
    seconds: Option<f64>,
    #[serde(default)]
    frames: Option<usize>,
    #[serde(default)]
    file_bytes: Option<usize>,
    #[serde(default)]
    infinite: Option<bool>,
    #[serde(default)]
    freq: Option<f64>,
}

pub(crate) fn parse_signal_request(
    kind: SignalKind,
    spec_with_ext: &str,
) -> Result<SignalRequest, SignalRequestError> {
    let (spec_b64, path_ext) = split_spec_and_ext(spec_with_ext)?;
    let payload = decode_signal_spec_payload(spec_b64)?;
    let format = resolve_signal_format(path_ext, payload.ext.as_deref())?;
    let spec = normalize_signal_spec(kind, format, &payload)?;
    Ok(SignalRequest { format, spec })
}

fn split_spec_and_ext(spec_with_ext: &str) -> Result<(&str, Option<&str>), SignalRequestError> {
    if spec_with_ext.is_empty() {
        return Err(SignalRequestError::InvalidBase64);
    }

    let Some((spec_b64, ext)) = spec_with_ext.rsplit_once('.') else {
        return Ok((spec_with_ext, None));
    };

    if spec_b64.is_empty() {
        return Err(SignalRequestError::InvalidBase64);
    }

    if ext.is_empty() {
        return Ok((spec_b64, None));
    }

    Ok((spec_b64, Some(ext)))
}

fn decode_signal_spec_payload(encoded: &str) -> Result<SignalSpecPayload, SignalRequestError> {
    if encoded.len() > Consts::MAX_SIGNAL_SPEC_BYTES {
        return Err(SignalRequestError::SpecTooLarge);
    }

    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .or_else(|_| URL_SAFE.decode(encoded))
        .map_err(|_| SignalRequestError::InvalidBase64)?;

    let payload: SignalSpecPayload =
        serde_json::from_slice(&bytes).map_err(|_| SignalRequestError::InvalidJson)?;

    Ok(payload)
}

fn resolve_signal_format(
    path_ext: Option<&str>,
    json_ext: Option<&str>,
) -> Result<SignalFormat, SignalRequestError> {
    match (path_ext, json_ext) {
        (Some(path_ext), Some(json_ext)) => {
            let path_format = SignalFormat::try_from(path_ext)?;
            let json_format = SignalFormat::try_from(json_ext)?;

            if path_format != json_format {
                return Err(SignalRequestError::InvalidField {
                    field: "ext",
                    message: "must match the path extension when both are provided",
                });
            }

            Ok(path_format)
        }
        (Some(path_ext), None) => SignalFormat::try_from(path_ext),
        (None, Some(json_ext)) => SignalFormat::try_from(json_ext),
        (None, None) => Err(SignalRequestError::MissingExtension),
    }
}

fn normalize_signal_spec(
    kind: SignalKind,
    format: SignalFormat,
    payload: &SignalSpecPayload,
) -> Result<ResolvedSignalSpec, SignalRequestError> {
    if !(Consts::MIN_SAMPLE_RATE..=Consts::MAX_SAMPLE_RATE).contains(&payload.sample_rate) {
        return Err(SignalRequestError::InvalidField {
            field: "sample_rate",
            message: "must be between 8000 and 192000 Hz",
        });
    }

    if payload.channels == 0 || payload.channels > Consts::MAX_CHANNELS {
        return Err(SignalRequestError::InvalidField {
            field: "channels",
            message: "must be between 1 and 8",
        });
    }

    let length = normalize_length(payload, format)?;

    let sine_freq_hz = match kind {
        SignalKind::Sine => {
            let Some(freq) = payload.freq else {
                return Err(SignalRequestError::InvalidField {
                    field: "freq",
                    message: "is required for `sine`",
                });
            };

            if !freq.is_finite() || freq <= 0.0 {
                return Err(SignalRequestError::InvalidField {
                    field: "freq",
                    message: "must be finite and > 0",
                });
            }

            Some(freq)
        }
        SignalKind::Sawtooth | SignalKind::SawtoothDescending | SignalKind::Silence => {
            if payload.freq.is_some() {
                return Err(SignalRequestError::InvalidField {
                    field: "freq",
                    message: "is not allowed for sawtooth, silence, or sawtooth-desc routes",
                });
            }

            None
        }
    };

    Ok(ResolvedSignalSpec {
        kind,
        sample_rate: payload.sample_rate,
        channels: payload.channels,
        length,
        sine_freq_hz,
    })
}

fn normalize_length(
    payload: &SignalSpecPayload,
    format: SignalFormat,
) -> Result<SignalLength, SignalRequestError> {
    let selected_count = usize::from(payload.seconds.is_some())
        + usize::from(payload.frames.is_some())
        + usize::from(payload.file_bytes.is_some())
        + usize::from(payload.infinite.is_some());

    if selected_count != 1 {
        return Err(SignalRequestError::InvalidField {
            field: "length",
            message: "exactly one of `seconds`, `frames`, `file_bytes`, or `infinite` must be set",
        });
    }

    if let Some(seconds) = payload.seconds {
        if !seconds.is_finite() || seconds <= 0.0 || seconds > Consts::MAX_SIGNAL_SECONDS {
            return Err(SignalRequestError::InvalidField {
                field: "seconds",
                message: "must be finite, > 0, and <= 300 seconds",
            });
        }

        let total_frames = (seconds * f64::from(payload.sample_rate)).floor() as usize;
        if total_frames == 0 {
            return Err(SignalRequestError::InvalidField {
                field: "seconds",
                message: "is too short for the requested sample_rate",
            });
        }

        return validate_pcm_budget(total_frames, payload.channels)
            .map(|()| SignalLength::from_frames(total_frames));
    }

    if let Some(total_frames) = payload.frames {
        if total_frames == 0 {
            return Err(SignalRequestError::InvalidField {
                field: "frames",
                message: "must be > 0",
            });
        }

        return validate_pcm_budget(total_frames, payload.channels)
            .map(|()| SignalLength::from_frames(total_frames));
    }

    if let Some(total_file_bytes) = payload.file_bytes {
        if format != SignalFormat::Wav {
            return Err(SignalRequestError::InvalidField {
                field: "file_bytes",
                message: "is currently only supported for `wav`",
            });
        }

        let data_bytes = total_file_bytes
            .checked_sub(Consts::WAV_HEADER_SIZE)
            .ok_or(SignalRequestError::InvalidField {
                field: "file_bytes",
                message: "must be at least 44 bytes for a WAV header",
            })?;

        let bytes_per_frame = payload.channels as usize * size_of::<i16>();
        if data_bytes % bytes_per_frame != 0 {
            return Err(SignalRequestError::InvalidLengthLayout);
        }

        let total_frames = data_bytes / bytes_per_frame;
        return validate_pcm_budget(total_frames, payload.channels)
            .map(|()| SignalLength::from_frames(total_frames));
    }

    if let Some(infinite) = payload.infinite {
        if !infinite {
            return Err(SignalRequestError::InvalidField {
                field: "infinite",
                message: "must be `true` when provided",
            });
        }

        if format != SignalFormat::Wav {
            return Err(SignalRequestError::InvalidField {
                field: "infinite",
                message: "is currently only supported for `wav`",
            });
        }

        return Ok(SignalLength::Infinite);
    }

    Err(SignalRequestError::InvalidField {
        field: "length",
        message: "missing length mode",
    })
}

fn validate_pcm_budget(total_frames: usize, channels: u16) -> Result<(), SignalRequestError> {
    let pcm_bytes = total_frames
        .checked_mul(channels as usize)
        .and_then(|samples| samples.checked_mul(size_of::<i16>()))
        .ok_or(SignalRequestError::PcmBudgetExceeded)?;
    if pcm_bytes > Consts::MAX_SIGNAL_PCM_BYTES {
        return Err(SignalRequestError::PcmBudgetExceeded);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kithara;

    fn encode(json: &str) -> String {
        URL_SAFE_NO_PAD.encode(json)
    }

    #[kithara::test]
    fn parses_supported_length_modes() {
        let cases = [
            (
                SignalKind::Sine,
                r#"{"seconds":1.5,"sample_rate":44100,"channels":2,"freq":440}"#,
                "wav",
                SignalLength::from_frames(66_150),
                Some(440.0),
            ),
            (
                SignalKind::Sawtooth,
                r#"{"seconds":1,"sample_rate":44100,"channels":2,"ext":"wav"}"#,
                "",
                SignalLength::from_frames(44_100),
                None,
            ),
            (
                SignalKind::Sawtooth,
                r#"{"frames":1024,"sample_rate":44100,"channels":2}"#,
                "wav",
                SignalLength::from_frames(1024),
                None,
            ),
            (
                SignalKind::Sawtooth,
                r#"{"file_bytes":4140,"sample_rate":44100,"channels":2}"#,
                "wav",
                SignalLength::from_frames(1024),
                None,
            ),
            (
                SignalKind::Sine,
                r#"{"infinite":true,"sample_rate":44100,"channels":2,"freq":440}"#,
                "wav",
                SignalLength::Infinite,
                Some(440.0),
            ),
            (
                SignalKind::Silence,
                r#"{"seconds":1,"sample_rate":44100,"channels":2}"#,
                "wav",
                SignalLength::from_frames(44_100),
                None,
            ),
        ];

        for (kind, json, ext, expected_length, expected_freq) in cases {
            let spec = encode(json);
            let spec_with_ext = if ext.is_empty() {
                spec
            } else {
                format!("{spec}.{ext}")
            };
            let request = parse_signal_request(kind, &spec_with_ext).unwrap();

            let expected_ext = if ext.is_empty() { "wav" } else { ext };
            assert_eq!(
                request.format,
                SignalFormat::try_from(expected_ext).unwrap()
            );
            assert_eq!(request.spec.kind, kind);
            assert_eq!(request.spec.sample_rate, 44_100);
            assert_eq!(request.spec.channels, 2);
            assert_eq!(request.spec.length, expected_length);
            assert_eq!(request.spec.sine_freq_hz, expected_freq);
        }
    }
}
