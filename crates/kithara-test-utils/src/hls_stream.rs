//! Packaged HLS orchestration: playlists and segment bytes are built from [`EncodedTrack`]
//! and fMP4 mux output. This module intentionally does not import `FFmpeg` types.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use aes::Aes128;
use cbc::{
    Encryptor,
    cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7},
};
use kithara_encode::{EncodeError, EncodedTrack, EncoderFactory, PackagedEncodeRequest, PcmSource};
use kithara_stream::MediaInfo;

use crate::{
    fixture_protocol::{create_wav_init_header, generate_segment},
    fmp4::{PackagedVariantData, mux_audio_track},
    hls_spec::{
        HlsSpecError, ResolvedDataMode, ResolvedEncryption, ResolvedHlsSpec, ResolvedInitMode,
        ResolvedPackagedAudioSpec, ResolvedPackagedSignal, ResolvedPackagedVariant,
    },
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_from_signal,
};

pub(crate) type GeneratedHlsCache = RwLock<HashMap<String, Arc<GeneratedHls>>>;

pub(crate) fn load_hls(
    cache: &GeneratedHlsCache,
    spec: ResolvedHlsSpec,
) -> Result<Arc<GeneratedHls>, HlsSpecError> {
    let cache_key = spec.cache_key().to_owned();
    {
        let cache = cache.read().expect("hls cache poisoned");
        if let Some(existing) = cache.get(&cache_key) {
            return Ok(Arc::clone(existing));
        }
    }

    let generated = Arc::new(GeneratedHls::new(spec)?);
    let mut cache = cache.write().expect("hls cache poisoned");
    Ok(Arc::clone(
        cache
            .entry(cache_key)
            .or_insert_with(|| Arc::clone(&generated)),
    ))
}

pub(crate) struct GeneratedHls {
    spec: ResolvedHlsSpec,
    master_playlist: String,
    media_playlists: Vec<String>,
    body: MaterializedHlsBody,
}

enum MaterializedDataMode {
    TestPattern,
    AbrBinary,
    SharedBytes(Arc<Vec<u8>>),
    PerVariantBytes(Vec<Arc<Vec<u8>>>),
}

enum MaterializedHlsBody {
    Legacy {
        data_mode: MaterializedDataMode,
        init_segments: Vec<Arc<Vec<u8>>>,
    },
    Packaged {
        variants: Vec<PackagedVariantData>,
    },
}

struct DelayPaddedPcm<'a> {
    inner: &'a dyn PcmSource,
    encoder_delay_frames: usize,
    trailing_delay_frames: usize,
}

#[derive(Debug)]
struct OffsetSignal<S> {
    inner: S,
    start_frame: usize,
}

impl<S> OffsetSignal<S> {
    fn new(inner: S, start_frame: u64) -> Self {
        Self {
            inner,
            start_frame: usize::try_from(start_frame).expect("start_frame must fit usize"),
        }
    }
}

impl<S> signal::SignalFn for OffsetSignal<S>
where
    S: signal::SignalFn,
{
    fn sample(&self, frame: usize, sample_rate: u32) -> i16 {
        self.inner
            .sample(frame.saturating_add(self.start_frame), sample_rate)
    }
}

impl DelayPaddedPcm<'_> {
    fn bytes_per_frame(&self) -> usize {
        usize::from(self.inner.channels()) * size_of::<i16>()
    }

    fn encoder_delay_bytes(&self) -> usize {
        self.encoder_delay_frames
            .saturating_mul(self.bytes_per_frame())
    }

    fn trailing_delay_bytes(&self) -> usize {
        self.trailing_delay_frames
            .saturating_mul(self.bytes_per_frame())
    }
}

impl PcmSource for DelayPaddedPcm<'_> {
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    fn channels(&self) -> u16 {
        self.inner.channels()
    }

    fn total_byte_len(&self) -> Option<usize> {
        self.inner.total_byte_len().map(|inner_len| {
            inner_len
                .saturating_add(self.encoder_delay_bytes())
                .saturating_add(self.trailing_delay_bytes())
        })
    }

    fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let Some(total_len) = self.total_byte_len() else {
            return 0;
        };
        if offset >= total_len || buf.is_empty() {
            return 0;
        }

        let writable = (total_len - offset).min(buf.len());
        let window = &mut buf[..writable];
        window.fill(0);

        let encoder_delay_bytes = self.encoder_delay_bytes();
        let inner_len = self.inner.total_byte_len().unwrap_or(0);
        let inner_start = encoder_delay_bytes;
        let inner_end = inner_start.saturating_add(inner_len);
        let copy_start = offset.max(inner_start);
        let copy_end = (offset + writable).min(inner_end);

        if copy_start < copy_end {
            let inner_offset = copy_start - inner_start;
            let dst_offset = copy_start - offset;
            let dst_end = dst_offset + (copy_end - copy_start);
            let _ = self
                .inner
                .read_pcm_at(inner_offset, &mut window[dst_offset..dst_end]);
        }

        writable
    }
}

impl GeneratedHls {
    fn new(spec: ResolvedHlsSpec) -> Result<Self, HlsSpecError> {
        let body = materialize_body(&spec)?;
        let master_playlist = build_master_playlist(&spec, &body);
        let media_playlists = (0..spec.variant_count)
            .map(|variant| build_media_playlist(&spec, &body, variant))
            .collect();

        Ok(Self {
            spec,
            master_playlist,
            media_playlists,
            body,
        })
    }

    pub(crate) fn master_playlist(&self, encoded_spec: &str) -> String {
        self.master_playlist.replace("{spec}", encoded_spec)
    }

    pub(crate) fn media_playlist(&self, variant: usize) -> Option<&str> {
        self.media_playlists.get(variant).map(String::as_str)
    }

    pub(crate) fn key_bytes(&self) -> Option<Vec<u8>> {
        self.spec
            .key_data
            .as_ref()
            .map(|bytes| bytes.as_slice().to_vec())
            .or_else(|| {
                self.spec
                    .encryption
                    .as_ref()
                    .map(|enc| enc.key.as_slice().to_vec())
            })
    }

    pub(crate) fn init_content_type(&self) -> Option<&'static str> {
        match self.body {
            MaterializedHlsBody::Legacy { .. } => None,
            MaterializedHlsBody::Packaged { .. } => Some("audio/mp4"),
        }
    }

    pub(crate) fn segment_content_type(&self) -> Option<&'static str> {
        match self.body {
            MaterializedHlsBody::Legacy { .. } => None,
            MaterializedHlsBody::Packaged { .. } => Some("audio/mp4"),
        }
    }

    pub(crate) fn init_bytes(&self, variant: usize) -> Option<Vec<u8>> {
        let plaintext = match &self.body {
            MaterializedHlsBody::Legacy { init_segments, .. } => {
                init_segments.get(variant)?.as_slice()
            }
            MaterializedHlsBody::Packaged { variants } => {
                variants.get(variant)?.init_segment.as_slice()
            }
        };
        Some(self.encrypt_if_needed(plaintext, 0))
    }

    pub(crate) fn segment_len(
        &self,
        variant: usize,
        segment: usize,
        use_head_override: bool,
    ) -> Option<usize> {
        self.segment_plaintext(variant, segment).map(|plaintext| {
            if use_head_override {
                self.spec
                    .head_reported_segment_size
                    .unwrap_or(plaintext.len())
            } else if self.spec.encryption.is_some() {
                plaintext.len() + (16 - plaintext.len() % 16)
            } else {
                plaintext.len()
            }
        })
    }

    pub(crate) fn segment_bytes(&self, variant: usize, segment: usize) -> Option<Vec<u8>> {
        let plaintext = self.segment_plaintext(variant, segment)?;
        Some(self.encrypt_if_needed(&plaintext, segment))
    }

    pub(crate) fn segment_delay_ms(&self, variant: usize, segment: usize) -> u64 {
        self.spec
            .delay_rules
            .iter()
            .find_map(|rule| rule.matches(variant, segment))
            .unwrap_or(0)
    }

    fn segment_plaintext(&self, variant: usize, segment: usize) -> Option<Vec<u8>> {
        if variant >= self.spec.variant_count {
            return None;
        }

        match &self.body {
            MaterializedHlsBody::Legacy { data_mode, .. } => {
                if segment >= self.spec.segments_per_variant {
                    return None;
                }
                let start = segment.checked_mul(self.spec.segment_size)?;
                match data_mode {
                    MaterializedDataMode::TestPattern => {
                        Some(generate_segment(variant, segment, self.spec.segment_size))
                    }
                    MaterializedDataMode::AbrBinary => {
                        Some(generate_abr_binary_segment(variant, segment))
                    }
                    MaterializedDataMode::SharedBytes(bytes) => {
                        let end = (start + self.spec.segment_size).min(bytes.len());
                        Some(bytes.get(start..end).unwrap_or(&[]).to_vec())
                    }
                    MaterializedDataMode::PerVariantBytes(per_variant) => {
                        let bytes = per_variant.get(variant)?;
                        let end = (start + self.spec.segment_size).min(bytes.len());
                        Some(bytes.get(start..end).unwrap_or(&[]).to_vec())
                    }
                }
            }
            MaterializedHlsBody::Packaged { variants } => variants
                .get(variant)?
                .media_segments
                .get(segment)
                .map(|segment| segment.as_slice().to_vec()),
        }
    }

    fn encrypt_if_needed(&self, data: &[u8], sequence: usize) -> Vec<u8> {
        let Some(enc) = &self.spec.encryption else {
            return data.to_vec();
        };
        let iv = derive_iv(enc, sequence);
        encrypt_aes128_cbc(data, &enc.key, &iv)
    }
}

fn materialize_body(spec: &ResolvedHlsSpec) -> Result<MaterializedHlsBody, HlsSpecError> {
    if let Some(packaged) = &spec.packaged_audio {
        let variants = packaged
            .variants
            .iter()
            .map(|variant| {
                encode_packaged_variant(packaged, variant)
                    .map_err(|error| HlsSpecError::PackagedAudio(error.to_string()))
                    .and_then(|track| {
                        mux_audio_track(&track)
                            .map_err(|error| HlsSpecError::PackagedAudio(error.to_string()))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(MaterializedHlsBody::Packaged { variants });
    }

    Ok(MaterializedHlsBody::Legacy {
        data_mode: materialize_data_mode(spec),
        init_segments: materialize_init_mode(spec),
    })
}

fn encode_packaged_variant(
    packaged: &ResolvedPackagedAudioSpec,
    variant: &ResolvedPackagedVariant,
) -> Result<EncodedTrack, EncodeError> {
    let encoder = EncoderFactory::create_packaged(packaged.codec)?;
    let frame_samples = encoder.packaged_frame_samples(packaged.codec)?;
    let requested_segment_frames =
        (packaged.segment_duration_secs * f64::from(packaged.sample_rate)).round() as usize;
    let packets_per_segment = requested_segment_frames.div_ceil(frame_samples).max(1);
    let content_frames = packets_per_segment
        .saturating_mul(frame_samples)
        .saturating_mul(packaged.segments_per_variant);
    let unaligned_total_frames = content_frames
        .saturating_add(packaged.encoder_delay as usize)
        .saturating_add(packaged.trailing_delay as usize);
    let total_frames = unaligned_total_frames
        .div_ceil(frame_samples)
        .saturating_mul(frame_samples);
    let aligned_trailing_delay = packaged.trailing_delay.saturating_add(
        u32::try_from(total_frames.saturating_sub(unaligned_total_frames)).unwrap_or(u32::MAX),
    );

    let media_info = MediaInfo::default()
        .with_codec(packaged.codec)
        .with_container(packaged.container)
        .with_sample_rate(packaged.sample_rate)
        .with_channels(packaged.channels);
    let content_length = Finite::new(content_frames);

    let encode = |pcm: &dyn PcmSource| {
        encoder.encode_packaged(PackagedEncodeRequest {
            pcm,
            media_info: media_info.clone(),
            timescale: packaged.timescale,
            bit_rate: variant.bit_rate,
            packets_per_segment,
            encoder_delay: packaged.encoder_delay,
            trailing_delay: aligned_trailing_delay,
        })
    };

    let encode_signal = |pcm: &dyn PcmSource| {
        let padded = DelayPaddedPcm {
            inner: pcm,
            encoder_delay_frames: packaged.encoder_delay as usize,
            trailing_delay_frames: aligned_trailing_delay as usize,
        };
        encode(&padded)
    };

    match variant.signal {
        ResolvedPackagedSignal::Sawtooth => {
            if variant.start_frame == 0 {
                let pcm = SignalPcm::new(
                    signal::Sawtooth,
                    packaged.sample_rate,
                    packaged.channels,
                    content_length,
                );
                encode_signal(&pcm)
            } else {
                let pcm = SignalPcm::new(
                    OffsetSignal::new(signal::Sawtooth, variant.start_frame),
                    packaged.sample_rate,
                    packaged.channels,
                    content_length,
                );
                encode_signal(&pcm)
            }
        }
        ResolvedPackagedSignal::SawtoothDescending => {
            if variant.start_frame == 0 {
                let pcm = SignalPcm::new(
                    signal::SawtoothDescending,
                    packaged.sample_rate,
                    packaged.channels,
                    content_length,
                );
                encode_signal(&pcm)
            } else {
                let pcm = SignalPcm::new(
                    OffsetSignal::new(signal::SawtoothDescending, variant.start_frame),
                    packaged.sample_rate,
                    packaged.channels,
                    content_length,
                );
                encode_signal(&pcm)
            }
        }
        ResolvedPackagedSignal::Silence => {
            if variant.start_frame == 0 {
                let pcm = SignalPcm::new(
                    signal::Silence,
                    packaged.sample_rate,
                    packaged.channels,
                    content_length,
                );
                encode_signal(&pcm)
            } else {
                let pcm = SignalPcm::new(
                    OffsetSignal::new(signal::Silence, variant.start_frame),
                    packaged.sample_rate,
                    packaged.channels,
                    content_length,
                );
                encode_signal(&pcm)
            }
        }
        ResolvedPackagedSignal::Sine { freq_hz } => {
            if variant.start_frame == 0 {
                let pcm = SignalPcm::new(
                    signal::SineWave(freq_hz),
                    packaged.sample_rate,
                    packaged.channels,
                    content_length,
                );
                encode_signal(&pcm)
            } else {
                let pcm = SignalPcm::new(
                    OffsetSignal::new(signal::SineWave(freq_hz), variant.start_frame),
                    packaged.sample_rate,
                    packaged.channels,
                    content_length,
                );
                encode_signal(&pcm)
            }
        }
        ResolvedPackagedSignal::Pattern(pattern) => {
            let pcm = SignalPcm::new(
                pattern,
                packaged.sample_rate,
                packaged.channels,
                content_length,
            );
            encode_signal(&pcm)
        }
    }
}

fn materialize_data_mode(spec: &ResolvedHlsSpec) -> MaterializedDataMode {
    match &spec.data_mode {
        ResolvedDataMode::TestPattern => MaterializedDataMode::TestPattern,
        ResolvedDataMode::AbrBinary => MaterializedDataMode::AbrBinary,
        ResolvedDataMode::SharedBytes(bytes) => {
            MaterializedDataMode::SharedBytes(Arc::clone(bytes))
        }
        ResolvedDataMode::PerVariantBytes(bytes) => {
            MaterializedDataMode::PerVariantBytes(bytes.clone())
        }
        ResolvedDataMode::SawWav {
            sample_rate,
            channels,
        } => {
            let wav = create_wav_from_signal(SignalPcm::new(
                signal::Sawtooth,
                *sample_rate,
                *channels,
                Finite::from_segments(spec.segments_per_variant, spec.segment_size, *channels),
            ));
            MaterializedDataMode::SharedBytes(Arc::new(wav))
        }
        ResolvedDataMode::PerVariantPcm {
            sample_rate,
            channels,
            patterns,
        } => {
            let bytes = (0..spec.variant_count)
                .map(|variant| {
                    let pattern = patterns
                        .get(variant)
                        .copied()
                        .unwrap_or(crate::fixture_protocol::PcmPattern::Ascending);
                    Arc::new(
                        SignalPcm::new(
                            pattern,
                            *sample_rate,
                            *channels,
                            Finite::from_segments(
                                spec.segments_per_variant,
                                spec.segment_size,
                                *channels,
                            ),
                        )
                        .into_vec(),
                    )
                })
                .collect();
            MaterializedDataMode::PerVariantBytes(bytes)
        }
    }
}

fn generate_abr_binary_segment(variant: usize, segment: usize) -> Vec<u8> {
    let total_len: usize = if variant == 2 && segment == 0 {
        50_000
    } else {
        200_000
    };
    let header_size = 1 + 4 + 4;
    let data_len = total_len.saturating_sub(header_size);

    let mut data = Vec::with_capacity(total_len);
    data.push(variant as u8);
    data.extend(&(segment as u32).to_be_bytes());
    data.extend(&(data_len as u32).to_be_bytes());
    data.extend(std::iter::repeat_n(b'A', data_len));
    data
}

fn materialize_init_mode(spec: &ResolvedHlsSpec) -> Vec<Arc<Vec<u8>>> {
    match &spec.init_mode {
        ResolvedInitMode::None => (0..spec.variant_count)
            .map(|_| Arc::new(Vec::new()))
            .collect(),
        ResolvedInitMode::TestInit => (0..spec.variant_count)
            .map(|variant| Arc::new(generate_test_init_segment(variant)))
            .collect(),
        ResolvedInitMode::WavHeader {
            sample_rate,
            channels,
        } => {
            let header = Arc::new(create_wav_init_header(*sample_rate, *channels));
            vec![header; spec.variant_count]
        }
        ResolvedInitMode::PerVariantBytes(data) => (0..spec.variant_count)
            .map(|variant| {
                data.get(variant)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(Vec::new()))
            })
            .collect(),
    }
}

fn generate_test_init_segment(variant: usize) -> Vec<u8> {
    let prefix = format!("V{variant}-INIT:");
    let mut data = prefix.into_bytes();
    data.extend(b"TEST_INIT_DATA");
    data
}

fn build_master_playlist(spec: &ResolvedHlsSpec, body: &MaterializedHlsBody) -> String {
    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:6\n");
    if matches!(body, MaterializedHlsBody::Packaged { .. }) {
        playlist.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
    }
    for (variant, bandwidth) in spec.variant_bandwidths.iter().copied().enumerate() {
        match body {
            MaterializedHlsBody::Legacy { .. } => playlist.push_str(&format!(
                "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}\n{{spec}}/v{variant}.m3u8\n"
            )),
            MaterializedHlsBody::Packaged { variants } => {
                let codecs = variants
                    .get(variant)
                    .map_or("mp4a.40.2", |variant| variant.rfc6381_codec.as_ref());
                playlist.push_str(&format!(
                    "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},CODECS=\"{codecs}\"\n{{spec}}/v{variant}.m3u8\n"
                ));
            }
        }
    }
    playlist
}

fn build_media_playlist(
    spec: &ResolvedHlsSpec,
    body: &MaterializedHlsBody,
    variant: usize,
) -> String {
    let target_duration = match body {
        MaterializedHlsBody::Legacy { .. } => spec.segment_duration_secs.ceil() as u64,
        MaterializedHlsBody::Packaged { variants } => variants.get(variant).map_or_else(
            || spec.segment_duration_secs.ceil() as u64,
            |variant| {
                variant
                    .segment_durations_secs
                    .iter()
                    .copied()
                    .fold(0.0_f64, f64::max)
                    .ceil() as u64
            },
        ),
    };
    let mut playlist = format!(
        "#EXTM3U\n\
         #EXT-X-VERSION:6\n\
         #EXT-X-TARGETDURATION:{}\n\
         #EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n",
        target_duration,
    );
    if spec.init_mode.is_present_for(variant)
        || matches!(body, MaterializedHlsBody::Packaged { .. })
    {
        playlist.push_str(&format!("#EXT-X-MAP:URI=\"init/v{variant}.mp4\"\n"));
    }
    if let Some(enc) = &spec.encryption {
        playlist.push_str("#EXT-X-KEY:METHOD=AES-128,URI=\"../key.bin\"");
        if let Some(iv) = enc.iv_hex() {
            playlist.push_str(&format!(",IV=0x{iv}"));
        }
        playlist.push('\n');
    }
    let segment_durations: Vec<f64> = match body {
        MaterializedHlsBody::Legacy { .. } => {
            vec![spec.segment_duration_secs; spec.segments_per_variant]
        }
        MaterializedHlsBody::Packaged { variants } => variants
            .get(variant)
            .map(|variant| variant.segment_durations_secs.clone())
            .unwrap_or_default(),
    };
    for (segment, duration) in segment_durations.iter().copied().enumerate() {
        playlist.push_str(&format!(
            "#EXTINF:{duration:.3},\nseg/v{variant}_{segment}.m4s\n"
        ));
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    playlist
}

impl ResolvedInitMode {
    fn is_present_for(&self, variant: usize) -> bool {
        match self {
            Self::None => false,
            Self::TestInit | Self::WavHeader { .. } => true,
            Self::PerVariantBytes(data) => data.get(variant).is_some_and(|bytes| !bytes.is_empty()),
        }
    }
}

fn derive_iv(enc: &ResolvedEncryption, sequence: usize) -> [u8; 16] {
    enc.iv.unwrap_or_else(|| {
        let mut iv = [0u8; 16];
        iv[8..16].copy_from_slice(&(sequence as u64).to_be_bytes());
        iv
    })
}

fn encrypt_aes128_cbc(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    let encryptor = Encryptor::<Aes128>::new(key.into(), iv.into());
    let padded_len = data.len() + (16 - data.len() % 16);
    let mut buf = vec![0u8; padded_len];
    buf[..data.len()].copy_from_slice(data);
    let ciphertext = encryptor
        .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
        .expect("encrypt_padded_mut");
    ciphertext.to_vec()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_stream::AudioCodec;

    use super::*;
    use crate::{
        fixture_protocol::{
            DataMode, EncryptionRequest, PackagedAudioRequest, PackagedAudioSource, PackagedSignal,
        },
        hls_spec::parse_hls_spec_with,
        hls_url::{HlsSpec, encode_hls_spec},
    };

    #[test]
    fn builds_master_and_media_playlist() {
        let spec =
            parse_hls_spec_with(&encode_hls_spec(&HlsSpec::default()), |_| unreachable!()).unwrap();
        let generated = GeneratedHls::new(spec).unwrap();
        assert!(
            generated
                .master_playlist("{encoded}")
                .contains("{encoded}/v0.m3u8")
        );
        assert!(
            generated
                .media_playlist(0)
                .unwrap()
                .contains("seg/v0_0.m4s")
        );
    }

    #[test]
    fn encrypts_segment_payload() {
        let spec = parse_hls_spec_with(
            &encode_hls_spec(&HlsSpec {
                segments_per_variant: 1,
                segment_size: 32,
                data_mode: DataMode::TestPattern,
                encryption: Some(EncryptionRequest {
                    key_hex: "30313233343536373839616263646566".to_string(),
                    iv_hex: Some("00000000000000000000000000000000".to_string()),
                }),
                ..HlsSpec::default()
            }),
            |_| unreachable!(),
        )
        .unwrap();
        let generated = GeneratedHls::new(spec).unwrap();
        let bytes = generated.segment_bytes(0, 0).unwrap();
        assert_ne!(bytes, generate_segment(0, 0, 32));
    }

    #[test]
    fn packaged_segments_can_exceed_requested_segment_count() {
        let spec = crate::test_server::HlsFixtureBuilder::new()
            .variant_count(1)
            .segments_per_variant(8)
            .segment_duration_secs(0.5)
            .packaged_audio_aac_lc(44_100, 2)
            .into_inline_spec();
        let resolved = crate::hls_spec::resolve_hls_spec_with(spec, |_| unreachable!()).unwrap();
        let generated = GeneratedHls::new(resolved).unwrap();
        let playlist = generated.media_playlist(0).unwrap();

        assert!(
            playlist.contains("seg/v0_8.m4s"),
            "packaged playlist should expose the muxed tail segment"
        );
        assert!(
            generated.segment_bytes(0, 8).is_some(),
            "packaged fixture must serve every segment listed in the playlist"
        );
    }

    #[test]
    fn packaged_delay_padding_can_extend_playlist_tail() {
        let spec = crate::test_server::HlsFixtureBuilder::new()
            .variant_count(1)
            .segments_per_variant(8)
            .segment_duration_secs(0.5)
            .packaged_audio(PackagedAudioRequest {
                codec: AudioCodec::AacLc,
                sample_rate: 44_100,
                channels: 2,
                start_frame: None,
                timescale: Some(44_100),
                bit_rate: Some(128_000),
                encoder_delay: NonZeroU32::new(2_112),
                trailing_delay: NonZeroU32::new(960),
                source: PackagedAudioSource::Signal(PackagedSignal::Sawtooth),
                variant_overrides: Vec::new(),
            })
            .into_inline_spec();
        let resolved = crate::hls_spec::resolve_hls_spec_with(spec, |_| unreachable!()).unwrap();
        let generated = GeneratedHls::new(resolved).unwrap();
        let playlist = generated.media_playlist(0).unwrap();

        assert!(
            playlist.contains("seg/v0_8.m4s"),
            "packaged delay padding should be able to push muxed audio into an extra tail segment"
        );
        assert!(generated.segment_bytes(0, 8).is_some());
    }
}
