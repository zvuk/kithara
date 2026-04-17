//! Spec-driven HTTP server for integration tests.
//!
//! Serves static repo assets, synthetic HLS (`/stream`), and procedural audio (`/signal`).
//! [`TestServerHelper`] (native) registers specs and returns ready-to-use URLs; callers
//! do not assemble tokens by hand. For a plain-language overview and run instructions,
//! see the crate `README.md`.
//!
//! Route families:
//! - `GET /health` — readiness probe for external runners
//! - `POST /token` — register spec payloads and return UUID tokens
//! - `GET /assets/{path...}` — static test assets
//! - `GET /signal/{form}/{spec_with_ext}` — procedural signal generation
//! - `GET /stream/{hls_spec}` — HLS stream generation

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
        DataMode, DelayRule, EncryptionRequest, InitMode, PackagedAudioRequest,
        PackagedAudioSource, PackagedSignal, PcmPattern,
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
    base_url: Url,
    token: String,
}

impl CreatedHls {
    pub(crate) fn new(base_url: Url, token: String) -> Self {
        Self { base_url, token }
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
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
    pub fn init_url(&self, variant: usize) -> Url {
        self.base_url
            .join(&hls_init_path_from_ref(&self.token, variant))
            .expect("join HLS init URL")
    }

    #[must_use]
    pub fn segment_url(&self, variant: usize, segment: usize) -> Url {
        self.base_url
            .join(&hls_segment_path_from_ref(&self.token, variant, segment))
            .expect("join HLS segment URL")
    }

    #[must_use]
    pub fn key_url(&self) -> Url {
        self.base_url
            .join(&hls_key_path_from_ref(&self.token))
            .expect("join HLS key URL")
    }
}

#[derive(Debug, Clone)]
pub struct HlsFixtureBuilder {
    spec: HlsSpec,
    data: HlsFixtureData,
    init: HlsFixtureInit,
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

    #[must_use]
    pub fn from_spec(spec: HlsSpec) -> Self {
        Self {
            data: HlsFixtureData::Spec(spec.data_mode.clone()),
            init: HlsFixtureInit::Spec(spec.init_mode.clone()),
            spec,
        }
    }

    #[must_use]
    pub fn variant_count(mut self, variant_count: usize) -> Self {
        self.spec.variant_count = variant_count;
        self
    }

    #[must_use]
    pub fn segments_per_variant(mut self, segments_per_variant: usize) -> Self {
        self.spec.segments_per_variant = segments_per_variant;
        self
    }

    #[must_use]
    pub fn segment_size(mut self, segment_size: usize) -> Self {
        self.spec.segment_size = segment_size;
        self
    }

    #[must_use]
    pub fn segment_duration_secs(mut self, segment_duration_secs: f64) -> Self {
        self.spec.segment_duration_secs = segment_duration_secs;
        self
    }

    #[must_use]
    pub fn data_mode(mut self, data_mode: DataMode) -> Self {
        self.clear_packaged_audio();
        self.spec.data_mode = data_mode.clone();
        self.data = HlsFixtureData::Spec(data_mode);
        self
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
    pub fn init_mode(mut self, init_mode: InitMode) -> Self {
        self.clear_packaged_audio();
        self.spec.init_mode = init_mode.clone();
        self.init = HlsFixtureInit::Spec(init_mode);
        self
    }

    #[must_use]
    pub fn init_data_per_variant(mut self, data: Vec<Arc<Vec<u8>>>) -> Self {
        self.clear_packaged_audio();
        self.init = HlsFixtureInit::PerVariantBytes(data);
        self
    }

    #[must_use]
    pub fn variant_bandwidths(mut self, variant_bandwidths: Vec<u64>) -> Self {
        self.spec.variant_bandwidths = Some(variant_bandwidths);
        self
    }

    #[must_use]
    pub fn delay_rules(mut self, delay_rules: Vec<DelayRule>) -> Self {
        self.spec.delay_rules = delay_rules;
        self
    }

    #[must_use]
    pub fn push_delay_rule(mut self, delay_rule: DelayRule) -> Self {
        self.spec.delay_rules.push(delay_rule);
        self
    }

    #[must_use]
    pub fn encryption(mut self, encryption: EncryptionRequest) -> Self {
        self.spec.encryption = Some(encryption);
        self
    }

    #[must_use]
    pub fn head_reported_segment_size(mut self, head_reported_segment_size: usize) -> Self {
        self.spec.head_reported_segment_size = Some(head_reported_segment_size);
        self
    }

    #[must_use]
    pub fn key_hex(mut self, key_hex: String) -> Self {
        self.spec.key_hex = Some(key_hex);
        self.spec.key_blob_ref = None;
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
    pub fn packaged_audio_sine_aac_lc(self, sample_rate: u32, channels: u16, freq_hz: f64) -> Self {
        self.packaged_audio_signal_aac_lc(sample_rate, channels, PackagedSignal::Sine { freq_hz })
    }

    #[must_use]
    pub fn packaged_audio_signal_aac_lc(
        mut self,
        sample_rate: u32,
        channels: u16,
        signal: PackagedSignal,
    ) -> Self {
        self.set_packaged_audio(PackagedAudioRequest {
            codec: AudioCodec::AacLc,
            sample_rate,
            channels,
            timescale: Some(sample_rate),
            bit_rate: Some(128_000),
            source: PackagedAudioSource::Signal(signal),
            variant_overrides: Vec::new(),
        });
        self
    }

    #[must_use]
    pub fn packaged_audio_per_variant_pcm_aac_lc(
        mut self,
        sample_rate: u32,
        channels: u16,
        patterns: Vec<PcmPattern>,
    ) -> Self {
        self.set_packaged_audio(PackagedAudioRequest {
            codec: AudioCodec::AacLc,
            sample_rate,
            channels,
            timescale: Some(sample_rate),
            bit_rate: Some(128_000),
            source: PackagedAudioSource::PerVariantPcm { patterns },
            variant_overrides: Vec::new(),
        });
        self
    }

    #[must_use]
    pub fn packaged_audio_flac(self, sample_rate: u32, channels: u16) -> Self {
        self.packaged_audio_signal_flac(sample_rate, channels, PackagedSignal::Sawtooth)
    }

    #[must_use]
    pub fn packaged_audio_sine_flac(self, sample_rate: u32, channels: u16, freq_hz: f64) -> Self {
        self.packaged_audio_signal_flac(sample_rate, channels, PackagedSignal::Sine { freq_hz })
    }

    #[must_use]
    pub fn packaged_audio_signal_flac(
        mut self,
        sample_rate: u32,
        channels: u16,
        signal: PackagedSignal,
    ) -> Self {
        self.set_packaged_audio(PackagedAudioRequest {
            codec: AudioCodec::Flac,
            sample_rate,
            channels,
            timescale: Some(sample_rate),
            bit_rate: Some(512_000),
            source: PackagedAudioSource::Signal(signal),
            variant_overrides: Vec::new(),
        });
        self
    }

    #[must_use]
    pub fn packaged_audio_per_variant_pcm_flac(
        mut self,
        sample_rate: u32,
        channels: u16,
        patterns: Vec<PcmPattern>,
    ) -> Self {
        self.set_packaged_audio(PackagedAudioRequest {
            codec: AudioCodec::Flac,
            sample_rate,
            channels,
            timescale: Some(sample_rate),
            bit_rate: Some(512_000),
            source: PackagedAudioSource::PerVariantPcm { patterns },
            variant_overrides: Vec::new(),
        });
        self
    }

    pub(crate) fn into_spec_with_blob_registrar<F>(self, register_blob: F) -> HlsSpec
    where
        F: Fn(&[u8]) -> String,
    {
        self.into_spec_inner(Some(&register_blob))
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by test-only helper paths")
    )]
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

    fn clear_packaged_audio(&mut self) {
        self.spec.packaged_audio = None;
    }

    fn set_packaged_audio(&mut self, packaged_audio: PackagedAudioRequest) {
        self.spec.data_mode = DataMode::TestPattern;
        self.spec.init_mode = InitMode::None;
        self.data = HlsFixtureData::Spec(DataMode::TestPattern);
        self.init = HlsFixtureInit::Spec(InitMode::None);
        self.spec.packaged_audio = Some(packaged_audio);
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
