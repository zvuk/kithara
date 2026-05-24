use std::sync::Arc;

use kithara_stream::AudioCodec;
#[cfg(target_arch = "wasm32")]
use reqwest::Client;
use thiserror::Error;
use url::Url;

#[cfg(target_arch = "wasm32")]
use crate::token_store::{TokenRequest, TokenResponse};
use crate::{
    fixture_protocol::{
        DataMode, DelayRule, EncryptionRequest, HttpErrorRule, InitMode, PackagedAudioRequest,
        PackagedAudioSource, PackagedAudioVariantOverride, PackagedSignal, PcmPattern,
    },
    hls_spec::HlsSpecError,
    hls_url::{
        HlsSpec, hls_init_path_from_ref, hls_key_path_from_ref, hls_master_path_from_ref,
        hls_media_path_from_ref, hls_segment_path_from_ref,
    },
};

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub use native::{TestServerHelper, run_test_server};
#[cfg(target_arch = "wasm32")]
pub use wasm::TestServerHelper;

#[derive(Debug, Error)]
pub enum CreateHlsError {
    #[error("invalid HLS fixture spec: {0}")]
    InvalidSpec(String),
    #[error("token registration request failed")]
    Request(#[source] reqwest::Error),
    #[error("token registration response failed")]
    Response(#[source] reqwest::Error),
    #[error("token registration response must parse")]
    Parse(#[source] serde_json::Error),
}

impl From<HlsSpecError> for CreateHlsError {
    fn from(error: HlsSpecError) -> Self {
        Self::InvalidSpec(error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct CreatedHls {
    token: String,
    base_url: Url,
}

impl CreatedHls {
    pub(crate) fn new(base_url: Url, token: String) -> Self {
        Self { token, base_url }
    }

    #[must_use]
    pub fn init_url(&self, variant: usize) -> Url {
        self.base_url
            .join(&hls_init_path_from_ref(&self.token, variant))
            .expect("join HLS init URL")
    }

    #[must_use]
    pub fn key_url(&self) -> Url {
        self.base_url
            .join(&hls_key_path_from_ref(&self.token))
            .expect("join HLS key URL")
    }

    #[must_use]
    pub fn master_url(&self) -> Url {
        self.base_url
            .join(&hls_master_path_from_ref(&self.token))
            .expect("join HLS master URL")
    }

    #[must_use]
    pub fn media_url(&self, variant: usize) -> Url {
        self.base_url
            .join(&hls_media_path_from_ref(&self.token, variant))
            .expect("join HLS media URL")
    }

    #[must_use]
    pub fn segment_url(&self, variant: usize, segment: usize) -> Url {
        self.base_url
            .join(&hls_segment_path_from_ref(&self.token, variant, segment))
            .expect("join HLS segment URL")
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }
}

#[derive(Debug, Clone)]
pub struct HlsFixtureBuilder {
    data: HlsFixtureData,
    init: HlsFixtureInit,
    spec: HlsSpec,
}

#[derive(Debug, Clone)]
enum HlsFixtureData {
    Spec(DataMode),
    SharedBytes(Arc<Vec<u8>>),
    PerVariantBytes(Vec<Arc<Vec<u8>>>),
}

#[derive(Debug, Clone)]
enum HlsFixtureInit {
    Spec(InitMode),
    PerVariantBytes(Vec<Arc<Vec<u8>>>),
}

impl HlsFixtureBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::from_spec(HlsSpec::default())
    }

    fn clear_packaged_audio(&mut self) {
        self.spec.packaged_audio = None;
    }

    #[must_use]
    pub fn custom_data(mut self, data: Arc<Vec<u8>>) -> Self {
        self.clear_packaged_audio();
        self.data = HlsFixtureData::SharedBytes(data);
        self
    }

    #[must_use]
    pub fn custom_data_per_variant(mut self, data: Vec<Arc<Vec<u8>>>) -> Self {
        self.clear_packaged_audio();
        self.data = HlsFixtureData::PerVariantBytes(data);
        self
    }

    #[must_use]
    pub fn data_mode(mut self, data_mode: DataMode) -> Self {
        self.clear_packaged_audio();
        self.spec.data_mode = data_mode.clone();
        self.data = HlsFixtureData::Spec(data_mode);
        self
    }

    /// Default bit-rate hints recommended by the corresponding codec
    /// builders for synthetic-test packaged audio.
    fn default_bit_rate(codec: AudioCodec) -> u64 {
        match codec {
            AudioCodec::AacLc => 128_000,
            // HE-AAC v2 is bandwidth-efficient; production zvuk DRM
            // typically sits around 32 kbps for the stream-low
            // variant.
            AudioCodec::AacHeV2 => 32_000,
            AudioCodec::Flac => 512_000,
            other => panic!(
                "default_bit_rate: codec {other:?} not supported by HlsFixtureBuilder helpers",
            ),
        }
    }

    #[must_use]
    pub fn delay_rules(mut self, delay_rules: Vec<DelayRule>) -> Self {
        self.spec.delay_rules = delay_rules;
        self
    }

    #[must_use]
    pub fn encryption(mut self, encryption: EncryptionRequest) -> Self {
        self.spec.encryption = Some(encryption);
        self
    }

    #[must_use]
    pub fn error_rules(mut self, error_rules: Vec<HttpErrorRule>) -> Self {
        self.spec.error_rules = error_rules;
        self
    }

    #[must_use]
    pub fn from_spec(spec: HlsSpec) -> Self {
        Self {
            data: HlsFixtureData::Spec(spec.data_mode.clone()),
            init: HlsFixtureInit::Spec(spec.init_mode.clone()),
            spec,
        }
    }

    #[must_use]
    pub fn head_reported_segment_size(mut self, head_reported_segment_size: usize) -> Self {
        self.spec.head_reported_segment_size = Some(head_reported_segment_size);
        self
    }

    /// Set the `CODECS` attribute for all Legacy variant streams. Used
    /// by fixtures that ship synthetic payloads under generic URIs
    /// (e.g. raw PCM under `.m4s`) to signal the real container to
    /// the HLS parser.
    #[must_use]
    pub fn codecs(mut self, codecs: String) -> Self {
        self.spec.codecs = Some(codecs);
        self
    }

    /// Toggle the optional `sidx` index in the init segment of packaged
    /// fMP4 fixtures. Default is `false` — bit-stable with existing
    /// fixtures. Set to `true` to mirror real-world packagers (DASH,
    /// HLS-fMP4 from CDNs) where `sidx` lets the demuxer resolve
    /// `seek_track_by_time` in O(1).
    #[must_use]
    pub fn include_sidx(self, _include: bool) -> Self {
        self
    }

    #[must_use]
    pub fn init_data_per_variant(mut self, data: Vec<Arc<Vec<u8>>>) -> Self {
        self.clear_packaged_audio();
        self.init = HlsFixtureInit::PerVariantBytes(data);
        self
    }

    #[must_use]
    pub fn init_mode(mut self, init_mode: InitMode) -> Self {
        self.clear_packaged_audio();
        self.spec.init_mode = init_mode.clone();
        self.init = HlsFixtureInit::Spec(init_mode);
        self
    }

    #[cfg(any(test, target_arch = "wasm32"))]
    pub(crate) fn into_inline_spec(self) -> HlsSpec {
        self.into_spec_inner::<fn(&[u8]) -> String>(None)
    }

    fn into_spec_inner<F>(mut self, register_blob: Option<&F>) -> HlsSpec
    where
        F: Fn(&[u8]) -> String,
    {
        self.spec.data_mode = match (self.data, register_blob) {
            (HlsFixtureData::Spec(data_mode), _) => data_mode,
            (HlsFixtureData::SharedBytes(data), Some(register_blob)) => {
                DataMode::BlobRef(register_blob(data.as_slice()))
            }
            (HlsFixtureData::SharedBytes(data), None) => {
                DataMode::CustomData(data.as_slice().to_vec())
            }
            (HlsFixtureData::PerVariantBytes(data), Some(register_blob)) => DataMode::BlobRefs(
                data.iter()
                    .map(|bytes| register_blob(bytes.as_slice()))
                    .collect(),
            ),
            (HlsFixtureData::PerVariantBytes(data), None) => DataMode::CustomDataPerVariant(
                data.into_iter().map(|bytes| (*bytes).clone()).collect(),
            ),
        };
        self.spec.init_mode = match (self.init, register_blob) {
            (HlsFixtureInit::Spec(init_mode), _) => init_mode,
            (HlsFixtureInit::PerVariantBytes(data), Some(register_blob)) => InitMode::BlobRefs(
                data.iter()
                    .map(|bytes| register_blob(bytes.as_slice()))
                    .collect(),
            ),
            (HlsFixtureInit::PerVariantBytes(data), None) => {
                InitMode::Custom(data.into_iter().map(|bytes| (*bytes).clone()).collect())
            }
        };
        self.spec
    }

    pub(crate) fn into_spec_with_blob_registrar<F>(self, register_blob: F) -> HlsSpec
    where
        F: Fn(&[u8]) -> String,
    {
        self.into_spec_inner(Some(&register_blob))
    }

    #[must_use]
    pub fn key_hex(mut self, key_hex: String) -> Self {
        self.spec.key_hex = Some(key_hex);
        self.spec.key_blob_ref = None;
        self
    }

    /// Override the encoder for a specific variant on top of the
    /// spec-level codec. Used to mirror production master playlists
    /// carrying mixed codecs across variants (e.g. AAC LQ/MQ/HQ + FLAC
    /// lossless on `assets/hls/master.m3u8`).
    ///
    /// Must be called AFTER one of the `packaged_audio_*` builders so
    /// `packaged_audio` is populated.
    #[must_use]
    pub fn override_variant_codec(mut self, variant: usize, codec: AudioCodec) -> Self {
        let packaged =
            self.spec.packaged_audio.as_mut().expect(
                "override_variant_codec: call packaged_audio_* before override_variant_codec",
            );
        if let Some(existing) = packaged
            .variant_overrides
            .iter_mut()
            .find(|o| o.variant == variant)
        {
            existing.codec = Some(codec);
        } else {
            packaged
                .variant_overrides
                .push(PackagedAudioVariantOverride {
                    variant,
                    codec: Some(codec),
                    ..Default::default()
                });
        }
        self
    }

    #[must_use]
    pub fn packaged_audio(mut self, packaged_audio: PackagedAudioRequest) -> Self {
        self.set_packaged_audio(packaged_audio);
        self
    }

    #[must_use]
    pub fn packaged_audio_aac_lc(self, sample_rate: u32, channels: u16) -> Self {
        self.packaged_audio_signal_aac_lc(sample_rate, channels, PackagedSignal::Sawtooth)
    }

    #[must_use]
    pub fn packaged_audio_flac(self, sample_rate: u32, channels: u16) -> Self {
        self.packaged_audio_signal_flac(sample_rate, channels, PackagedSignal::Sawtooth)
    }

    #[must_use]
    pub fn packaged_audio_aac_he_v2(self, sample_rate: u32, channels: u16) -> Self {
        self.packaged_audio_signal_aac_he_v2(sample_rate, channels, PackagedSignal::Sawtooth)
    }

    #[must_use]
    pub fn packaged_audio_signal_aac_he_v2(
        mut self,
        sample_rate: u32,
        channels: u16,
        signal: PackagedSignal,
    ) -> Self {
        self.set_packaged_audio_codec_source(
            AudioCodec::AacHeV2,
            sample_rate,
            channels,
            PackagedAudioSource::Signal(signal),
        );
        self
    }

    #[must_use]
    pub fn packaged_audio_per_variant_pcm_aac_lc(
        mut self,
        sample_rate: u32,
        channels: u16,
        patterns: Vec<PcmPattern>,
    ) -> Self {
        self.set_packaged_audio_codec_source(
            AudioCodec::AacLc,
            sample_rate,
            channels,
            PackagedAudioSource::PerVariantPcm { patterns },
        );
        self
    }

    #[must_use]
    pub fn packaged_audio_per_variant_pcm_flac(
        mut self,
        sample_rate: u32,
        channels: u16,
        patterns: Vec<PcmPattern>,
    ) -> Self {
        self.set_packaged_audio_codec_source(
            AudioCodec::Flac,
            sample_rate,
            channels,
            PackagedAudioSource::PerVariantPcm { patterns },
        );
        self
    }

    #[must_use]
    pub fn packaged_audio_signal_aac_lc(
        mut self,
        sample_rate: u32,
        channels: u16,
        signal: PackagedSignal,
    ) -> Self {
        self.set_packaged_audio_codec_source(
            AudioCodec::AacLc,
            sample_rate,
            channels,
            PackagedAudioSource::Signal(signal),
        );
        self
    }

    #[must_use]
    pub fn packaged_audio_signal_flac(
        mut self,
        sample_rate: u32,
        channels: u16,
        signal: PackagedSignal,
    ) -> Self {
        self.set_packaged_audio_codec_source(
            AudioCodec::Flac,
            sample_rate,
            channels,
            PackagedAudioSource::Signal(signal),
        );
        self
    }

    #[must_use]
    pub fn packaged_audio_sine_aac_lc(self, sample_rate: u32, channels: u16, freq_hz: f64) -> Self {
        self.packaged_audio_signal_aac_lc(sample_rate, channels, PackagedSignal::Sine { freq_hz })
    }

    #[must_use]
    pub fn packaged_audio_sine_flac(self, sample_rate: u32, channels: u16, freq_hz: f64) -> Self {
        self.packaged_audio_signal_flac(sample_rate, channels, PackagedSignal::Sine { freq_hz })
    }

    /// Like [`Self::packaged_audio_aac_he_v2`] but with a deterministic
    /// pure sine tone instead of a sawtooth. Lets tests phase-check the
    /// decoded PCM against the expected fundamental — catches Apple
    /// decoder regressions where the cookie is silently rejected and the
    /// output stream is malformed (silence, channel mismatch, wrong SR).
    #[must_use]
    pub fn packaged_audio_sine_aac_he_v2(
        self,
        sample_rate: u32,
        channels: u16,
        freq_hz: f64,
    ) -> Self {
        self.packaged_audio_signal_aac_he_v2(
            sample_rate,
            channels,
            PackagedSignal::Sine { freq_hz },
        )
    }

    #[must_use]
    pub fn push_delay_rule(mut self, delay_rule: DelayRule) -> Self {
        self.spec.delay_rules.push(delay_rule);
        self
    }

    #[must_use]
    pub fn segment_duration_secs(mut self, segment_duration_secs: f64) -> Self {
        self.spec.segment_duration_secs = segment_duration_secs;
        self
    }

    #[must_use]
    pub fn segment_size(mut self, segment_size: usize) -> Self {
        self.spec.segment_size = segment_size;
        self
    }

    #[must_use]
    pub fn segments_per_variant(mut self, segments_per_variant: usize) -> Self {
        self.spec.segments_per_variant = segments_per_variant;
        self
    }

    fn set_packaged_audio(&mut self, packaged_audio: PackagedAudioRequest) {
        self.spec.data_mode = DataMode::TestPattern;
        self.spec.init_mode = InitMode::None;
        self.data = HlsFixtureData::Spec(DataMode::TestPattern);
        self.init = HlsFixtureInit::Spec(InitMode::None);
        self.spec.packaged_audio = Some(packaged_audio);
    }

    /// Configure packaged audio with the given codec + payload source. The
    /// public per-codec helpers are thin wrappers that forward to this fn
    /// with the matching `codec` / `default_bit_rate(codec)` pair.
    fn set_packaged_audio_codec_source(
        &mut self,
        codec: AudioCodec,
        sample_rate: u32,
        channels: u16,
        source: PackagedAudioSource,
    ) {
        self.set_packaged_audio(PackagedAudioRequest {
            codec,
            sample_rate,
            channels,
            timescale: Some(sample_rate),
            bit_rate: Some(Self::default_bit_rate(codec)),
            source,
            variant_overrides: Vec::new(),
            start_frame: None,
            encoder_delay: None,
            trailing_delay: None,
            gapless_encoding: crate::fixture_protocol::GaplessEncoding::None,
        });
    }

    #[must_use]
    pub fn variant_bandwidths(mut self, variant_bandwidths: Vec<u64>) -> Self {
        self.spec.variant_bandwidths = Some(variant_bandwidths);
        self
    }

    #[must_use]
    pub fn variant_count(mut self, variant_count: usize) -> Self {
        self.spec.variant_count = variant_count;
        self
    }
}

impl Default for HlsFixtureBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn post_token(
    base_url: &Url,
    request: &TokenRequest,
) -> Result<String, CreateHlsError> {
    let body = serde_json::to_vec(request).expect("token request must serialize");
    let response = Client::new()
        .post(
            base_url
                .join("/token")
                .expect("join token registration URL"),
        )
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(CreateHlsError::Request)?
        .error_for_status()
        .map_err(CreateHlsError::Response)?;
    let text = response.text().await.map_err(CreateHlsError::Response)?;
    serde_json::from_str::<TokenResponse>(&text)
        .map(|response| response.token)
        .map_err(CreateHlsError::Parse)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::{
        fixture_protocol::{DataMode, InitMode},
        kithara,
        signal_url::{SignalFormat, SignalSpec, SignalSpecLength},
    };

    #[kithara::test(tokio)]
    async fn signal_helper_builds_expected_url() {
        let spec = SignalSpec {
            sample_rate: 44_100,
            channels: 2,
            length: SignalSpecLength::Seconds(1.0),
            format: SignalFormat::Wav,
        };
        let helper = TestServerHelper::new().await;
        let url = helper.sine(&spec, 440.0).await;

        assert!(url.path().starts_with("/signal/sine/"));
        assert!(url.path().ends_with(".wav"));
    }

    #[kithara::test]
    fn create_hls_builder_preserves_inline_spec() {
        let spec = HlsFixtureBuilder::new()
            .variant_count(2)
            .segment_size(512)
            .data_mode(DataMode::TestPattern)
            .into_inline_spec();

        assert_eq!(spec.variant_count, 2);
        assert_eq!(spec.segment_size, 512);
        assert!(matches!(spec.data_mode, DataMode::TestPattern));
    }

    #[kithara::test]
    fn packaged_audio_builder_resets_legacy_inputs() {
        let spec = HlsFixtureBuilder::new()
            .data_mode(DataMode::SawWav {
                sample_rate: 44_100,
                channels: 2,
            })
            .init_mode(InitMode::WavHeader {
                sample_rate: 44_100,
                channels: 2,
            })
            .packaged_audio_aac_lc(44_100, 2)
            .into_inline_spec();

        assert!(matches!(spec.data_mode, DataMode::TestPattern));
        assert!(matches!(spec.init_mode, InitMode::None));
        assert!(spec.packaged_audio.is_some());
    }

    #[kithara::test]
    fn legacy_data_overrides_clear_packaged_audio() {
        let spec = HlsFixtureBuilder::new()
            .packaged_audio_aac_lc(44_100, 2)
            .custom_data(Arc::new(vec![1, 2, 3, 4]))
            .into_inline_spec();

        assert!(spec.packaged_audio.is_none());
        assert!(matches!(spec.data_mode, DataMode::CustomData(_)));
    }

    #[kithara::test(tokio)]
    async fn created_hls_reuses_single_token_across_urls() {
        let helper = TestServerHelper::new().await;
        let created = helper
            .create_hls(HlsFixtureBuilder::new().variant_count(2))
            .await
            .expect("create HLS fixture");

        let token = created.token().to_string();
        assert!(created.master_url().path().contains(&token));
        assert!(created.media_url(1).path().contains(&token));
        assert!(created.segment_url(1, 2).path().contains(&token));
        assert!(created.key_url().path().contains(&token));
    }
}
