use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::fixture_protocol::{
    DataMode, DelayRule, EncryptionRequest, InitMode, PackagedAudioRequest,
};

fn default_variant_count() -> usize {
    1
}

fn default_segments_per_variant() -> usize {
    3
}

fn default_segment_size() -> usize {
    200_000
}

fn default_segment_duration_secs() -> f64 {
    4.0
}

fn default_data_mode() -> DataMode {
    DataMode::TestPattern
}

fn default_init_mode() -> InitMode {
    InitMode::None
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires fn(&T)"
)]
fn is_default_variant_count(value: &usize) -> bool {
    *value == default_variant_count()
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires fn(&T)"
)]
fn is_default_segments_per_variant(value: &usize) -> bool {
    *value == default_segments_per_variant()
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires fn(&T)"
)]
fn is_default_segment_size(value: &usize) -> bool {
    *value == default_segment_size()
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires fn(&T)"
)]
fn is_default_segment_duration_secs(value: &f64) -> bool {
    (*value - default_segment_duration_secs()).abs() < f64::EPSILON
}

fn is_default_data_mode(value: &DataMode) -> bool {
    matches!(value, DataMode::TestPattern)
}

fn is_default_init_mode(value: &InitMode) -> bool {
    matches!(value, InitMode::None)
}

/// Public request shape for `/stream/*` URL construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HlsSpec {
    #[serde(
        default = "default_data_mode",
        skip_serializing_if = "is_default_data_mode"
    )]
    pub data_mode: DataMode,
    #[serde(
        default = "default_init_mode",
        skip_serializing_if = "is_default_init_mode"
    )]
    pub init_mode: InitMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_reported_segment_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_blob_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packaged_audio: Option<PackagedAudioRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant_bandwidths: Option<Vec<u64>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delay_rules: Vec<DelayRule>,
    #[serde(
        default = "default_segment_duration_secs",
        skip_serializing_if = "is_default_segment_duration_secs"
    )]
    pub segment_duration_secs: f64,
    #[serde(
        default = "default_segment_size",
        skip_serializing_if = "is_default_segment_size"
    )]
    pub segment_size: usize,
    #[serde(
        default = "default_segments_per_variant",
        skip_serializing_if = "is_default_segments_per_variant"
    )]
    pub segments_per_variant: usize,
    #[serde(
        default = "default_variant_count",
        skip_serializing_if = "is_default_variant_count"
    )]
    pub variant_count: usize,
}

impl Default for HlsSpec {
    fn default() -> Self {
        Self {
            variant_count: default_variant_count(),
            segments_per_variant: default_segments_per_variant(),
            segment_size: default_segment_size(),
            segment_duration_secs: default_segment_duration_secs(),
            data_mode: default_data_mode(),
            init_mode: InitMode::None,
            variant_bandwidths: None,
            delay_rules: Vec::new(),
            encryption: None,
            head_reported_segment_size: None,
            key_hex: None,
            key_blob_ref: None,
            packaged_audio: None,
        }
    }
}

#[must_use]
pub fn encode_hls_spec(spec: &HlsSpec) -> String {
    let json = serde_json::to_vec(spec).expect("hls spec must serialize");
    URL_SAFE_NO_PAD.encode(json)
}

/// Build a `/stream/{spec}.m3u8` master playlist path.
#[must_use]
pub fn hls_master_path(spec: &HlsSpec) -> String {
    hls_master_path_from_ref(&encode_hls_spec(spec))
}

/// Build a `/stream/{spec_ref}.m3u8` master playlist path.
#[must_use]
pub fn hls_master_path_from_ref(spec_ref: &str) -> String {
    format!("/stream/{spec_ref}.m3u8")
}

/// Build a `/stream/{spec}/v{variant}.m3u8` media playlist path.
#[must_use]
pub fn hls_media_path(spec: &HlsSpec, variant: usize) -> String {
    hls_media_path_from_ref(&encode_hls_spec(spec), variant)
}

/// Build a `/stream/{spec_ref}/v{variant}.m3u8` media playlist path.
#[must_use]
pub fn hls_media_path_from_ref(spec_ref: &str, variant: usize) -> String {
    format!("/stream/{spec_ref}/v{variant}.m3u8")
}

/// Build a `/stream/{spec}/init/v{variant}.mp4` init path.
#[must_use]
pub fn hls_init_path(spec: &HlsSpec, variant: usize) -> String {
    hls_init_path_from_ref(&encode_hls_spec(spec), variant)
}

/// Build a `/stream/{spec_ref}/init/v{variant}.mp4` init path.
#[must_use]
pub fn hls_init_path_from_ref(spec_ref: &str, variant: usize) -> String {
    format!("/stream/{spec_ref}/init/v{variant}.mp4")
}

/// Build a `/stream/{spec}/seg/v{variant}_{segment}.m4s` segment path.
#[must_use]
pub fn hls_segment_path(spec: &HlsSpec, variant: usize, segment: usize) -> String {
    hls_segment_path_from_ref(&encode_hls_spec(spec), variant, segment)
}

/// Build a `/stream/{spec_ref}/seg/v{variant}_{segment}.m4s` segment path.
#[must_use]
pub fn hls_segment_path_from_ref(spec_ref: &str, variant: usize, segment: usize) -> String {
    format!("/stream/{spec_ref}/seg/v{variant}_{segment}.m4s")
}

/// Build a `/stream/{spec}/key.bin` key path.
#[must_use]
pub fn hls_key_path(spec: &HlsSpec) -> String {
    hls_key_path_from_ref(&encode_hls_spec(spec))
}

/// Build a `/stream/{spec_ref}/key.bin` key path.
#[must_use]
pub fn hls_key_path_from_ref(spec_ref: &str) -> String {
    format!("/stream/{spec_ref}/key.bin")
}
