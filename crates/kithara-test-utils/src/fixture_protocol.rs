//! Shared protocol types for synthetic HLS test fixtures.
//!
//! These types are transport-agnostic and contain no platform-specific
//! dependencies beyond `serde`.
//!
//! # Data Generation
//!
//! Pure functions for segment/WAV data generation are also defined here, so
//! both server (generates data) and client (computes `expected_byte_at`)
//! share the exact same logic.

use kithara_stream::AudioCodec;
use serde::{Deserialize, Serialize};

use crate::{signal_pcm::signal, wav::create_wav_header};

/// Serde for [`AudioCodec`] as `snake_case` strings (matches the former
/// `derive(Serialize)` on the enum).
mod serde_audio_codec {
    use kithara_stream::AudioCodec;
    use serde::{Deserialize, Deserializer, Serializer, de::Error as DeError};

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with = module requires fn(&T, serializer)"
    )]
    pub(super) fn serialize<S>(codec: &AudioCodec, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s: &'static str = match codec {
            AudioCodec::AacLc => "aac_lc",
            AudioCodec::AacHe => "aac_he",
            AudioCodec::AacHeV2 => "aac_he_v2",
            AudioCodec::Mp3 => "mp3",
            AudioCodec::Flac => "flac",
            AudioCodec::Vorbis => "vorbis",
            AudioCodec::Opus => "opus",
            AudioCodec::Alac => "alac",
            AudioCodec::Pcm => "pcm",
            AudioCodec::Adpcm => "adpcm",
        };
        serializer.serialize_str(s)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<AudioCodec, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "aac_lc" => Ok(AudioCodec::AacLc),
            "aac_he" => Ok(AudioCodec::AacHe),
            "aac_he_v2" => Ok(AudioCodec::AacHeV2),
            "mp3" => Ok(AudioCodec::Mp3),
            "flac" => Ok(AudioCodec::Flac),
            "vorbis" => Ok(AudioCodec::Vorbis),
            "opus" => Ok(AudioCodec::Opus),
            "alac" => Ok(AudioCodec::Alac),
            "pcm" => Ok(AudioCodec::Pcm),
            "adpcm" => Ok(AudioCodec::Adpcm),
            _ => Err(DeError::custom(format!("unknown audio codec string: {s}"))),
        }
    }
}

/// How media segment data is generated.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum DataMode {
    /// `"V{v}-SEG-{s}:TEST_SEGMENT_DATA"` prefix + `0xFF` padding.
    TestPattern,
    /// Saw-tooth WAV audio data.
    SawWav { sample_rate: u32, channels: u16 },
    /// Per-variant PCM data (no WAV header — init segment provides it).
    PerVariantPcm {
        sample_rate: u32,
        channels: u16,
        patterns: Vec<PcmPattern>,
    },
    /// Exact custom segment bytes for all variants.
    CustomData(Vec<u8>),
    /// Exact custom segment bytes for each variant.
    CustomDataPerVariant(Vec<Vec<u8>>),
    /// Shared media payload stored out-of-band and referenced by key.
    BlobRef(String),
    /// Per-variant media payloads stored out-of-band and referenced by keys.
    BlobRefs(Vec<String>),
    /// Compatibility-only ABR binary payload format used by historical integration tests.
    ///
    /// Prefer `PackagedAudioRequest` for new audio HLS fixtures.
    AbrBinary,
}

/// PCM saw-tooth pattern for a variant.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub enum PcmPattern {
    /// Ascending saw-tooth: frame 0 → -32768, frame 65535 → 32767.
    Ascending,
    /// Descending saw-tooth: frame 0 → 32767, frame 65535 → -32768.
    Descending,
    /// Ascending saw-tooth with half-period phase offset.
    ShiftedAscending,
}

impl signal::SignalFn for PcmPattern {
    fn sample(&self, frame: usize, sample_rate: u32) -> i16 {
        match self {
            Self::Ascending => signal::Sawtooth.sample(frame, sample_rate),
            Self::Descending => signal::SawtoothDescending.sample(frame, sample_rate),
            Self::ShiftedAscending => signal::SawtoothShifted.sample(frame, sample_rate),
        }
    }
}

/// How init segments are generated.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum InitMode {
    /// No init segments.
    None,
    /// Compatibility-only fixed-init payload: `V{variant}-INIT:TEST_INIT_DATA`.
    ///
    /// Prefer packaged init segments for new audio HLS fixtures.
    TestInit,
    /// 44-byte WAV header (streaming mode: size = 0xFFFFFFFF).
    WavHeader { sample_rate: u32, channels: u16 },
    /// Exact custom init bytes for each variant.
    ///
    /// Variant `v` uses `data[v]`; missing entries produce an empty init segment.
    Custom(Vec<Vec<u8>>),
    /// Per-variant init bytes stored out-of-band and referenced by keys.
    BlobRefs(Vec<String>),
}

/// Audio source description for packaged fMP4 HLS fixtures.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum PackagedAudioSource {
    /// Single procedural signal used for every variant unless overridden.
    Signal(PackagedSignal),
    /// Direct per-variant PCM patterns.
    PerVariantPcm { patterns: Vec<PcmPattern> },
}

/// Base procedural signal for packaged audio fixtures.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub enum PackagedSignal {
    Sawtooth,
    SawtoothDescending,
    Silence,
    Sine { freq_hz: f64 },
}

/// Per-variant override for packaged audio fixtures.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PackagedAudioVariantOverride {
    pub variant: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<PcmPattern>,
}

/// Preferred description for real audio fMP4 packaging in new audio HLS fixtures.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackagedAudioRequest {
    #[serde(with = "serde_audio_codec")]
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timescale: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<u64>,
    pub source: PackagedAudioSource,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variant_overrides: Vec<PackagedAudioVariantOverride>,
}

/// Declarative delay rule for segment serving.
///
/// All conditions that are `Some` must match for the rule to apply.
/// The first matching rule wins; if none matches, the delay is zero.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DelayRule {
    /// Match only this variant index. `None` = any variant.
    pub variant: Option<usize>,
    /// Match only this exact segment index. `None` = any segment.
    pub segment_eq: Option<usize>,
    /// Match segments with index >= N. `None` = no lower bound.
    pub segment_gte: Option<usize>,
    /// Delay in milliseconds.
    pub delay_ms: u64,
}

impl DelayRule {
    /// Evaluate this rule against a given variant and segment index.
    /// Returns `Some(delay_ms)` if the rule matches, `None` otherwise.
    #[must_use]
    pub fn matches(&self, variant: usize, segment: usize) -> Option<u64> {
        if let Some(v) = self.variant
            && v != variant
        {
            return None;
        }
        if let Some(eq) = self.segment_eq
            && eq != segment
        {
            return None;
        }
        if let Some(gte) = self.segment_gte
            && segment < gte
        {
            return None;
        }
        Some(self.delay_ms)
    }
}

/// Evaluate delay rules: returns the `delay_ms` of the first matching rule, or 0.
#[must_use]
pub fn eval_delay(rules: &[DelayRule], variant: usize, segment: usize) -> u64 {
    rules
        .iter()
        .find_map(|r| r.matches(variant, segment))
        .unwrap_or(0)
}

/// Encryption parameters for HLS segments.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EncryptionRequest {
    /// 16-byte AES key as hex string.
    pub key_hex: String,
    /// Optional 16-byte IV as hex string. When `None`, derived from segment sequence.
    pub iv_hex: Option<String>,
}

/// Generate test-pattern segment data: `V{v}-SEG-{s}:TEST_SEGMENT_DATA` + `0xFF` padding.
#[must_use]
pub fn generate_segment(variant: usize, segment: usize, size: usize) -> Vec<u8> {
    let prefix = format!("V{variant}-SEG-{segment}:");
    let mut data = prefix.into_bytes();
    data.extend(b"TEST_SEGMENT_DATA");
    if data.len() < size {
        data.resize(size, 0xFF);
    }
    data
}

/// Compute expected byte at a global offset for `TestPattern` data mode.
///
/// Byte stream layout: `[init_data][media_seg_0][media_seg_1]...[media_seg_N]`
#[must_use]
pub fn expected_byte_at_test_pattern(
    variant: usize,
    offset: u64,
    init_len: u64,
    segment_size: usize,
) -> u8 {
    if offset < init_len {
        // Caller must handle init region separately (data depends on InitMode).
        return 0;
    }

    let media_offset = offset - init_len;
    let seg_idx = (media_offset / segment_size as u64) as usize;
    let off_in_seg = (media_offset % segment_size as u64) as usize;

    let prefix = format!("V{variant}-SEG-{seg_idx}:TEST_SEGMENT_DATA");
    let prefix_bytes = prefix.as_bytes();

    if off_in_seg < prefix_bytes.len() {
        prefix_bytes[off_in_seg]
    } else {
        0xFF
    }
}

/// Create a 44-byte WAV init segment header (streaming mode: sizes = 0xFFFFFFFF).
#[must_use]
pub fn create_wav_init_header(sample_rate: u32, channels: u16) -> Vec<u8> {
    create_wav_header(sample_rate, channels, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kithara;

    #[kithara::test]
    fn packaged_audio_codec_json_uses_snake_case_strings() {
        let value = serde_json::json!({
            "codec": "aac_lc",
            "sample_rate": 44100,
            "channels": 2,
            "source": { "Signal": "Sawtooth" }
        });
        let req: PackagedAudioRequest = serde_json::from_value(value).unwrap();
        assert_eq!(req.codec, AudioCodec::AacLc);
        assert_eq!(req.sample_rate, 44100);
        assert_eq!(req.channels, 2);
    }

    #[kithara::test]
    fn packaged_audio_sine_signal_json_roundtrips() {
        let value = serde_json::json!({
            "codec": "aac_lc",
            "sample_rate": 44100,
            "channels": 2,
            "source": { "Signal": { "Sine": { "freq_hz": 440.0 } } }
        });
        let req: PackagedAudioRequest = serde_json::from_value(value).unwrap();
        assert_eq!(req.codec, AudioCodec::AacLc);
        assert!(matches!(
            req.source,
            PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz: 440.0 })
        ));
    }

    #[kithara::test]
    fn delay_rule_matches_variant_and_segment_gte() {
        let rule = DelayRule {
            variant: Some(0),
            segment_gte: Some(3),
            delay_ms: 500,
            ..Default::default()
        };
        assert_eq!(rule.matches(0, 2), None);
        assert_eq!(rule.matches(0, 3), Some(500));
        assert_eq!(rule.matches(0, 10), Some(500));
        assert_eq!(rule.matches(1, 5), None);
    }

    #[kithara::test]
    fn eval_delay_first_match_wins() {
        let rules = vec![
            DelayRule {
                variant: Some(0),
                segment_gte: Some(3),
                delay_ms: 500,
                ..Default::default()
            },
            DelayRule {
                delay_ms: 10,
                ..Default::default()
            },
        ];
        assert_eq!(eval_delay(&rules, 0, 5), 500);
        assert_eq!(eval_delay(&rules, 1, 0), 10);
        assert_eq!(eval_delay(&rules, 0, 0), 10);
    }

    #[kithara::test]
    fn generate_segment_has_correct_prefix() {
        let data = generate_segment(1, 2, 100);
        assert!(data.starts_with(b"V1-SEG-2:TEST_SEGMENT_DATA"));
        assert_eq!(data.len(), 100);
        assert_eq!(data[99], 0xFF);
    }

    #[kithara::test]
    fn wav_init_header_is_44_bytes() {
        let header = create_wav_init_header(44100, 2);
        assert_eq!(header.len(), 44);
        assert!(header.starts_with(b"RIFF"));
    }
}
