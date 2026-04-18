use std::{fmt, sync::Arc};

use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use kithara_stream::{AudioCodec, ContainerFormat, audio_codec_supports_fmp4_packaging};
use thiserror::Error;

use crate::{
    consts::Consts,
    fixture_protocol::{
        DataMode, DelayRule, EncryptionRequest, InitMode, PackagedAudioRequest,
        PackagedAudioSource, PackagedAudioVariantOverride, PackagedSignal, PcmPattern,
    },
    hls_url::HlsSpec,
};

#[derive(Debug, Clone)]
pub(crate) struct ResolvedHlsSpec {
    pub(crate) variant_count: usize,
    pub(crate) segments_per_variant: usize,
    pub(crate) segment_size: usize,
    pub(crate) segment_duration_secs: f64,
    pub(crate) data_mode: ResolvedDataMode,
    pub(crate) init_mode: ResolvedInitMode,
    pub(crate) variant_bandwidths: Vec<u64>,
    pub(crate) delay_rules: Vec<DelayRule>,
    pub(crate) encryption: Option<ResolvedEncryption>,
    pub(crate) head_reported_segment_size: Option<usize>,
    pub(crate) key_data: Option<Arc<Vec<u8>>>,
    pub(crate) packaged_audio: Option<ResolvedPackagedAudioSpec>,
    cache_key: String,
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedDataMode {
    TestPattern,
    AbrBinary,
    SharedBytes(Arc<Vec<u8>>),
    PerVariantBytes(Vec<Arc<Vec<u8>>>),
    SawWav {
        sample_rate: u32,
        channels: u16,
    },
    PerVariantPcm {
        sample_rate: u32,
        channels: u16,
        patterns: Vec<PcmPattern>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedInitMode {
    None,
    TestInit,
    WavHeader { sample_rate: u32, channels: u16 },
    PerVariantBytes(Vec<Arc<Vec<u8>>>),
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedEncryption {
    pub(crate) key: [u8; 16],
    pub(crate) iv: Option<[u8; 16]>,
    iv_hex: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedPackagedAudioSpec {
    pub(crate) codec: AudioCodec,
    pub(crate) container: ContainerFormat,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) timescale: u32,
    pub(crate) segments_per_variant: usize,
    pub(crate) segment_duration_secs: f64,
    pub(crate) variants: Vec<ResolvedPackagedVariant>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedPackagedVariant {
    pub(crate) bit_rate: u64,
    pub(crate) signal: ResolvedPackagedSignal,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ResolvedPackagedSignal {
    Sawtooth,
    SawtoothDescending,
    Silence,
    Sine { freq_hz: f64 },
    Pattern(PcmPattern),
}

#[derive(Debug, Error)]
pub(crate) enum HlsSpecError {
    #[error("hls spec is too large")]
    SpecTooLarge,
    #[error("hls spec is not valid base64url")]
    InvalidBase64,
    #[error("hls spec is not valid JSON")]
    InvalidJson,
    #[error("invalid hls spec field `{field}`: {message}")]
    InvalidField {
        field: &'static str,
        message: &'static str,
    },
    #[error("hls blob `{0}` is not registered")]
    MissingBlob(String),
    #[error("invalid encryption key or iv")]
    InvalidEncryption,
    #[error("unsupported packaged audio codec `{0:?}`")]
    UnsupportedPackagedCodec(AudioCodec),
    #[error("failed to materialize packaged audio: {0}")]
    PackagedAudio(String),
}

impl ResolvedHlsSpec {
    pub(crate) fn cache_key(&self) -> &str {
        &self.cache_key
    }
}

pub(crate) fn parse_hls_spec_with<F>(
    encoded: &str,
    resolve_blob: F,
) -> Result<ResolvedHlsSpec, HlsSpecError>
where
    F: Fn(&str) -> Result<Arc<Vec<u8>>, HlsSpecError>,
{
    if encoded.len() > Consts::MAX_HLS_SPEC_BYTES {
        return Err(HlsSpecError::SpecTooLarge);
    }

    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .or_else(|_| URL_SAFE.decode(encoded))
        .map_err(|_| HlsSpecError::InvalidBase64)?;
    let spec: HlsSpec = serde_json::from_slice(&bytes).map_err(|_| HlsSpecError::InvalidJson)?;
    resolve_hls_spec_with(spec, resolve_blob)
}

pub(crate) fn resolve_hls_spec_with<F>(
    spec: HlsSpec,
    resolve_blob: F,
) -> Result<ResolvedHlsSpec, HlsSpecError>
where
    F: Fn(&str) -> Result<Arc<Vec<u8>>, HlsSpecError>,
{
    validate_hls_shape(&spec)?;

    let variant_count = spec.variant_count;
    let mut variant_bandwidths = Vec::with_capacity(variant_count);
    for variant in 0..variant_count {
        let bandwidth = spec
            .variant_bandwidths
            .as_ref()
            .and_then(|values| values.get(variant).copied())
            .unwrap_or((variant + 1) as u64 * 1_280_000);
        variant_bandwidths.push(bandwidth);
    }

    let data_mode = normalize_data_mode(&spec.data_mode, &resolve_blob)?;
    let init_mode = normalize_init_mode(&spec.init_mode, &resolve_blob)?;
    let encryption = spec
        .encryption
        .as_ref()
        .map(normalize_encryption)
        .transpose()?;
    let packaged_audio = spec
        .packaged_audio
        .as_ref()
        .map(|packaged| resolve_packaged_audio(packaged, &spec))
        .transpose()?;
    let key_data = match (spec.key_hex.as_deref(), spec.key_blob_ref.as_deref()) {
        (Some(_), Some(_)) => {
            return Err(HlsSpecError::InvalidField {
                field: "key",
                message: "use either `key_hex` or `key_blob_ref`, not both",
            });
        }
        (Some(key_hex), None) => Some(Arc::new(
            hex::decode(key_hex).map_err(|_| HlsSpecError::InvalidEncryption)?,
        )),
        (None, Some(key_ref)) => Some(resolve_blob(key_ref)?),
        (None, None) => None,
    };
    let cache_key = build_cache_key(&spec);

    Ok(ResolvedHlsSpec {
        variant_count,
        segments_per_variant: spec.segments_per_variant,
        segment_size: spec.segment_size,
        segment_duration_secs: spec.segment_duration_secs,
        data_mode,
        init_mode,
        variant_bandwidths,
        delay_rules: spec.delay_rules,
        encryption,
        head_reported_segment_size: spec.head_reported_segment_size,
        key_data,
        packaged_audio,
        cache_key,
    })
}

fn validate_hls_shape(spec: &HlsSpec) -> Result<(), HlsSpecError> {
    if spec.variant_count == 0 || spec.variant_count > Consts::MAX_HLS_VARIANTS {
        return Err(HlsSpecError::InvalidField {
            field: "variant_count",
            message: "must be between 1 and 16",
        });
    }
    if spec.segments_per_variant == 0
        || spec.segments_per_variant > Consts::MAX_HLS_SEGMENTS_PER_VARIANT
    {
        return Err(HlsSpecError::InvalidField {
            field: "segments_per_variant",
            message: "must be between 1 and 4096",
        });
    }
    if spec.segment_size == 0 || spec.segment_size > Consts::MAX_HLS_SEGMENT_SIZE {
        return Err(HlsSpecError::InvalidField {
            field: "segment_size",
            message: "must be between 1 byte and 8 MiB",
        });
    }
    if !spec.segment_duration_secs.is_finite()
        || spec.segment_duration_secs <= 0.0
        || spec.segment_duration_secs > Consts::MAX_HLS_DURATION_SECS
    {
        return Err(HlsSpecError::InvalidField {
            field: "segment_duration_secs",
            message: "must be finite, > 0, and <= 600 seconds",
        });
    }
    if let Some(head_size) = spec.head_reported_segment_size
        && head_size == 0
    {
        return Err(HlsSpecError::InvalidField {
            field: "head_reported_segment_size",
            message: "must be > 0 when provided",
        });
    }
    if spec.packaged_audio.is_some()
        && (!matches!(spec.data_mode, DataMode::TestPattern)
            || !matches!(spec.init_mode, InitMode::None))
    {
        return Err(HlsSpecError::InvalidField {
            field: "packaged_audio",
            message: "requires default data_mode and init_mode",
        });
    }
    Ok(())
}

fn normalize_data_mode<F>(
    data_mode: &DataMode,
    resolve_blob: &F,
) -> Result<ResolvedDataMode, HlsSpecError>
where
    F: Fn(&str) -> Result<Arc<Vec<u8>>, HlsSpecError>,
{
    match data_mode {
        DataMode::TestPattern => Ok(ResolvedDataMode::TestPattern),
        DataMode::AbrBinary => Ok(ResolvedDataMode::AbrBinary),
        DataMode::CustomData(data) => Ok(ResolvedDataMode::SharedBytes(Arc::new(data.clone()))),
        DataMode::CustomDataPerVariant(data) => Ok(ResolvedDataMode::PerVariantBytes(
            data.iter().cloned().map(Arc::new).collect(),
        )),
        DataMode::BlobRef(key) => resolve_blob(key).map(ResolvedDataMode::SharedBytes),
        DataMode::BlobRefs(keys) => keys
            .iter()
            .map(|key| resolve_blob(key))
            .collect::<Result<Vec<_>, _>>()
            .map(ResolvedDataMode::PerVariantBytes),
        DataMode::SawWav {
            sample_rate,
            channels,
        } => {
            validate_pcm_shape(*sample_rate, *channels)?;
            Ok(ResolvedDataMode::SawWav {
                sample_rate: *sample_rate,
                channels: *channels,
            })
        }
        DataMode::PerVariantPcm {
            sample_rate,
            channels,
            patterns,
        } => {
            validate_pcm_shape(*sample_rate, *channels)?;
            Ok(ResolvedDataMode::PerVariantPcm {
                sample_rate: *sample_rate,
                channels: *channels,
                patterns: patterns.clone(),
            })
        }
    }
}

fn normalize_init_mode<F>(
    init_mode: &InitMode,
    resolve_blob: &F,
) -> Result<ResolvedInitMode, HlsSpecError>
where
    F: Fn(&str) -> Result<Arc<Vec<u8>>, HlsSpecError>,
{
    match init_mode {
        InitMode::None => Ok(ResolvedInitMode::None),
        InitMode::TestInit => Ok(ResolvedInitMode::TestInit),
        InitMode::Custom(data) => Ok(ResolvedInitMode::PerVariantBytes(
            data.iter().cloned().map(Arc::new).collect(),
        )),
        InitMode::BlobRefs(keys) => keys
            .iter()
            .map(|key| resolve_blob(key))
            .collect::<Result<Vec<_>, _>>()
            .map(ResolvedInitMode::PerVariantBytes),
        InitMode::WavHeader {
            sample_rate,
            channels,
        } => {
            validate_pcm_shape(*sample_rate, *channels)?;
            Ok(ResolvedInitMode::WavHeader {
                sample_rate: *sample_rate,
                channels: *channels,
            })
        }
    }
}

fn validate_pcm_shape(sample_rate: u32, channels: u16) -> Result<(), HlsSpecError> {
    if !(Consts::MIN_SAMPLE_RATE..=Consts::MAX_SAMPLE_RATE).contains(&sample_rate) {
        return Err(HlsSpecError::InvalidField {
            field: "sample_rate",
            message: "must be between 8000 and 192000 Hz",
        });
    }
    if channels == 0 || channels > Consts::MAX_CHANNELS {
        return Err(HlsSpecError::InvalidField {
            field: "channels",
            message: "must be between 1 and 8",
        });
    }
    Ok(())
}

fn resolve_packaged_audio(
    packaged: &PackagedAudioRequest,
    spec: &HlsSpec,
) -> Result<ResolvedPackagedAudioSpec, HlsSpecError> {
    validate_pcm_shape(packaged.sample_rate, packaged.channels)?;
    if !audio_codec_supports_fmp4_packaging(packaged.codec) {
        return Err(HlsSpecError::UnsupportedPackagedCodec(packaged.codec));
    }

    let timescale = packaged.timescale.unwrap_or(packaged.sample_rate);
    if timescale == 0 {
        return Err(HlsSpecError::InvalidField {
            field: "packaged_audio.timescale",
            message: "must be > 0",
        });
    }
    if matches!(packaged.codec, AudioCodec::AacLc) && timescale != packaged.sample_rate {
        return Err(HlsSpecError::InvalidField {
            field: "packaged_audio.timescale",
            message: "AAC-LC currently requires timescale == sample_rate",
        });
    }

    let base_signal = match &packaged.source {
        PackagedAudioSource::Signal(signal) => resolved_signal(*signal, packaged.sample_rate)?,
        PackagedAudioSource::PerVariantPcm { .. } => {
            ResolvedPackagedSignal::Pattern(PcmPattern::Ascending)
        }
    };
    let mut variants = Vec::with_capacity(spec.variant_count);
    for variant in 0..spec.variant_count {
        let mut bit_rate = packaged.bit_rate.unwrap_or(128_000);
        let mut signal = match &packaged.source {
            PackagedAudioSource::Signal(_) => base_signal,
            PackagedAudioSource::PerVariantPcm { patterns } => ResolvedPackagedSignal::Pattern(
                patterns
                    .get(variant)
                    .copied()
                    .unwrap_or(PcmPattern::Ascending),
            ),
        };
        if let Some(override_spec) = packaged
            .variant_overrides
            .iter()
            .find(|override_spec| override_spec.variant == variant)
        {
            apply_variant_override(override_spec, &mut bit_rate, &mut signal);
        }
        variants.push(ResolvedPackagedVariant { bit_rate, signal });
    }

    Ok(ResolvedPackagedAudioSpec {
        codec: packaged.codec,
        container: ContainerFormat::Fmp4,
        sample_rate: packaged.sample_rate,
        channels: packaged.channels,
        timescale,
        segments_per_variant: spec.segments_per_variant,
        segment_duration_secs: spec.segment_duration_secs,
        variants,
    })
}

fn apply_variant_override(
    override_spec: &PackagedAudioVariantOverride,
    bit_rate: &mut u64,
    signal: &mut ResolvedPackagedSignal,
) {
    if let Some(override_rate) = override_spec.bit_rate {
        *bit_rate = override_rate;
    }
    if let Some(pattern) = override_spec.pattern {
        *signal = ResolvedPackagedSignal::Pattern(pattern);
    }
}

fn resolved_signal(
    signal: PackagedSignal,
    sample_rate: u32,
) -> Result<ResolvedPackagedSignal, HlsSpecError> {
    match signal {
        PackagedSignal::Sawtooth => Ok(ResolvedPackagedSignal::Sawtooth),
        PackagedSignal::SawtoothDescending => Ok(ResolvedPackagedSignal::SawtoothDescending),
        PackagedSignal::Silence => Ok(ResolvedPackagedSignal::Silence),
        PackagedSignal::Sine { freq_hz } => {
            validate_packaged_sine_freq_hz(freq_hz, sample_rate)?;
            Ok(ResolvedPackagedSignal::Sine { freq_hz })
        }
    }
}

fn validate_packaged_sine_freq_hz(freq_hz: f64, sample_rate: u32) -> Result<(), HlsSpecError> {
    if !freq_hz.is_finite() || freq_hz <= 0.0 {
        return Err(HlsSpecError::InvalidField {
            field: "packaged_audio.source",
            message: "sine freq_hz must be finite and > 0",
        });
    }
    let nyquist = f64::from(sample_rate) / 2.0;
    if freq_hz > nyquist {
        return Err(HlsSpecError::InvalidField {
            field: "packaged_audio.source",
            message: "sine freq_hz must be <= half the sample rate (Nyquist)",
        });
    }
    Ok(())
}

fn normalize_encryption(enc: &EncryptionRequest) -> Result<ResolvedEncryption, HlsSpecError> {
    let key_bytes = hex::decode(&enc.key_hex).map_err(|_| HlsSpecError::InvalidEncryption)?;
    let key: [u8; 16] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| HlsSpecError::InvalidEncryption)?;
    let iv = enc
        .iv_hex
        .as_ref()
        .map(|value| {
            let bytes = hex::decode(value).map_err(|_| HlsSpecError::InvalidEncryption)?;
            let iv: [u8; 16] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| HlsSpecError::InvalidEncryption)?;
            Ok(iv)
        })
        .transpose()?;

    Ok(ResolvedEncryption {
        key,
        iv,
        iv_hex: enc.iv_hex.clone(),
    })
}

fn build_cache_key(spec: &HlsSpec) -> String {
    struct Stable<'a>(&'a HlsSpec);

    impl fmt::Display for Stable<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let json = serde_json::to_string(self.0).map_err(|_| fmt::Error)?;
            f.write_str(&json)
        }
    }

    Stable(spec).to_string()
}

impl ResolvedEncryption {
    pub(crate) fn iv_hex(&self) -> Option<&str> {
        self.iv_hex.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use kithara_stream::AudioCodec;

    use super::*;
    use crate::{
        fixture_protocol::{InitMode, PackagedAudioRequest, PackagedAudioSource, PackagedSignal},
        hls_blob_store::blob_key,
        kithara,
    };

    fn encode(spec: &HlsSpec) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(spec).unwrap())
    }

    fn resolve_from(
        map: &HashMap<String, Arc<Vec<u8>>>,
        key: &str,
    ) -> Result<Arc<Vec<u8>>, HlsSpecError> {
        map.get(key)
            .cloned()
            .ok_or_else(|| HlsSpecError::MissingBlob(key.to_owned()))
    }

    #[kithara::test]
    fn parses_blob_backed_spec() {
        let media = blob_key(b"abcdef");
        let init = blob_key(b"init");
        let key = blob_key(b"key");
        let mut blobs = HashMap::new();
        blobs.insert(media.clone(), Arc::new(b"abcdef".to_vec()));
        blobs.insert(init.clone(), Arc::new(b"init".to_vec()));
        blobs.insert(key.clone(), Arc::new(b"key".to_vec()));
        let spec = HlsSpec {
            variant_count: 2,
            data_mode: DataMode::BlobRef(media.clone()),
            init_mode: InitMode::BlobRefs(vec![init.clone(), init]),
            key_blob_ref: Some(key),
            ..HlsSpec::default()
        };

        let resolved =
            parse_hls_spec_with(&encode(&spec), |blob| resolve_from(&blobs, blob)).unwrap();
        assert_eq!(resolved.variant_count, 2);
        assert_eq!(resolved.variant_bandwidths, vec![1_280_000, 2_560_000]);
        match resolved.data_mode {
            ResolvedDataMode::SharedBytes(bytes) => assert_eq!(bytes.as_slice(), b"abcdef"),
            _ => panic!("expected shared bytes"),
        }
    }

    #[kithara::test]
    fn rejects_invalid_variant_count() {
        let spec = HlsSpec {
            variant_count: 0,
            ..HlsSpec::default()
        };
        let err = parse_hls_spec_with(&encode(&spec), |_| unreachable!()).unwrap_err();
        assert!(err.to_string().contains("variant_count"));
    }

    #[kithara::test]
    fn rejects_packaged_sine_freq_above_nyquist() {
        let spec = HlsSpec {
            packaged_audio: Some(PackagedAudioRequest {
                codec: AudioCodec::AacLc,
                sample_rate: 44_100,
                channels: 2,
                timescale: None,
                bit_rate: None,
                source: PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz: 50_000.0 }),
                variant_overrides: Vec::new(),
            }),
            ..HlsSpec::default()
        };
        let err = parse_hls_spec_with(&encode(&spec), |_| unreachable!()).unwrap_err();
        assert!(err.to_string().contains("Nyquist"));
    }
}
