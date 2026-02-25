//! Serializable protocol types for fixture server sessions.
//!
//! These types are shared between the fixture server (native binary) and
//! fixture clients (both native and WASM). They contain no platform-specific
//! dependencies — only `serde` for serialization.
//!
//! # Data Generation
//!
//! Pure functions for segment/WAV data generation are also defined here so
//! both server (generates data) and client (computes `expected_byte_at`)
//! share the exact same logic.

use serde::{Deserialize, Serialize};

// ── Session Configs ────────────────────────────────────────────────

/// Configuration for an HLS test session (maps to `HlsTestServer`).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HlsSessionConfig {
    /// Number of HLS variants (bitrate levels). Default: 1.
    pub variant_count: usize,
    /// Number of segments per variant. Default: 3.
    pub segments_per_variant: usize,
    /// Segment size in bytes. Default: 200 000 (200 KB).
    pub segment_size: usize,
    /// Segment duration in seconds (for playlist `#EXTINF`). Default: 4.0.
    pub segment_duration_secs: f64,
    /// How media segment data is generated.
    pub data_mode: DataMode,
    /// How init segments are generated.
    pub init_mode: InitMode,
    /// Custom bandwidths for master playlist variants.
    pub variant_bandwidths: Option<Vec<u64>>,
    /// Delay rules applied when serving segments.
    pub delay_rules: Vec<DelayRule>,
    /// AES-128 encryption config.
    pub encryption: Option<EncryptionRequest>,
    /// Size reported by HEAD responses for segments (simulates compressed size).
    /// When set, HEAD returns this value as Content-Length instead of `segment_size`.
    /// Used to test HEAD/GET size mismatch (e.g. HTTP auto-decompression).
    pub head_reported_segment_size: Option<usize>,
}

impl Default for HlsSessionConfig {
    fn default() -> Self {
        Self {
            variant_count: 1,
            segments_per_variant: 3,
            segment_size: 200_000,
            segment_duration_secs: 4.0,
            data_mode: DataMode::TestPattern,
            init_mode: InitMode::None,
            variant_bandwidths: None,
            delay_rules: Vec::new(),
            encryption: None,
            head_reported_segment_size: None,
        }
    }
}

/// Configuration for an ABR test session (maps to `AbrTestServer`).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AbrSessionConfig {
    /// Master playlist content.
    pub master_playlist: String,
    /// Whether to include init segments (`#EXT-X-MAP`).
    pub has_init: bool,
    /// Delay in ms for segment `v2_0`.
    pub segment0_delay_ms: u64,
}

/// Configuration for a fixed HLS test session (maps to `TestServer`).
///
/// No parameters — the server generates fixed content (3 variants × 3 segments).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FixedHlsSessionConfig;

/// Configuration for an audio fixtures session.
///
/// Serves embedded audio files (silence.wav, test.mp3) for decode tests.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AudioFixturesSessionConfig;

/// Configuration for a file download test session.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileSessionConfig {
    /// Named files to serve, with their content.
    pub files: Vec<FileEntry>,
    /// If true, first sequential GET response closes after `close_after_bytes` bytes.
    pub partial_close: Option<PartialCloseConfig>,
}

/// A file entry served by the file test session.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileEntry {
    /// Filename (path component, e.g. "audio.mp3")
    pub name: String,
    /// File content as bytes.
    pub data: Vec<u8>,
    /// Content-Type header.
    pub content_type: String,
}

/// Configuration for partial close behavior.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PartialCloseConfig {
    /// Close the initial sequential stream after this many bytes.
    pub close_after_bytes: usize,
    /// Total advertised size (Content-Length in HEAD).
    pub total_size: usize,
}

/// Configuration for an HTTP test session (generic endpoint testing).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HttpTestSessionConfig {
    /// Routes to register.
    pub routes: Vec<HttpTestRoute>,
}

/// A single route in an HTTP test session.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HttpTestRoute {
    /// Path (e.g. "/test" -- served at /s/{id}/http/test).
    pub path: String,
    /// HTTP status code to return.
    pub status: u16,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Response body (empty if None).
    pub body: Option<Vec<u8>>,
    /// Delay before responding, in milliseconds.
    pub delay_ms: Option<u64>,
    /// Whether to support Range requests on this route.
    pub support_range: bool,
    /// Number of initial requests to fail with 500 before succeeding.
    /// Used for retry testing.
    pub fail_first_n: Option<usize>,
}

// ── Data Mode ──────────────────────────────────────────────────────

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
}

/// PCM saw-tooth pattern for a variant.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum PcmPattern {
    /// Ascending saw-tooth: frame 0 → -32768, frame 65535 → 32767.
    Ascending,
    /// Descending saw-tooth: frame 0 → 32767, frame 65535 → -32768.
    Descending,
    /// Ascending saw-tooth with half-period phase offset.
    ShiftedAscending,
}

// ── Init Mode ──────────────────────────────────────────────────────

/// How init segments are generated.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum InitMode {
    /// No init segments.
    None,
    /// 44-byte WAV header (streaming mode: size = 0xFFFFFFFF).
    WavHeader { sample_rate: u32, channels: u16 },
}

// ── Delay Rules ────────────────────────────────────────────────────

/// Declarative delay rule for segment serving.
///
/// All conditions that are `Some` must match for the rule to apply.
/// First matching rule wins; if none match, delay is zero.
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

// ── Encryption ─────────────────────────────────────────────────────

/// Encryption parameters for HLS segments.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EncryptionRequest {
    /// 16-byte AES key as hex string.
    pub key_hex: String,
    /// Optional 16-byte IV as hex string. When `None`, derived from segment sequence.
    pub iv_hex: Option<String>,
}

// ── Session Response ───────────────────────────────────────────────

/// Response returned after creating a session.
#[derive(Serialize, Deserialize, Debug)]
pub struct SessionResponse {
    /// Unique session identifier.
    pub session_id: String,
    /// Base URL for this session's content (e.g. `http://127.0.0.1:3333/s/{id}`).
    pub base_url: String,
    /// Total bytes across all segments for one variant (including init).
    pub total_bytes: u64,
    /// Init segment length for variant 0 (0 if no init segments).
    pub init_len: u64,
}

// ── Data Generation (pure functions) ───────────────────────────────

/// Saw-tooth period in samples.
pub const SAW_PERIOD: usize = 65536;

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

/// Generate a PCM sample for a given frame and pattern.
#[must_use]
pub fn pcm_sample(pattern: &PcmPattern, frame: usize) -> i16 {
    match pattern {
        PcmPattern::Ascending => ((frame % SAW_PERIOD) as i32 - 32768) as i16,
        PcmPattern::Descending => (32767 - (frame % SAW_PERIOD) as i32) as i16,
        PcmPattern::ShiftedAscending => {
            (((frame + SAW_PERIOD / 2) % SAW_PERIOD) as i32 - 32768) as i16
        }
    }
}

/// Generate PCM segment data (no WAV header) for a given pattern.
#[must_use]
pub fn create_pcm_segments(
    pattern: &PcmPattern,
    channels: u16,
    segment_count: usize,
    segment_size: usize,
) -> Vec<u8> {
    let total_bytes = segment_count * segment_size;
    let bytes_per_frame = channels as usize * 2;
    let total_frames = total_bytes / bytes_per_frame;

    let mut pcm = Vec::with_capacity(total_bytes);
    for frame in 0..total_frames {
        let sample = pcm_sample(pattern, frame);
        for _ in 0..channels {
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
    }
    pcm.resize(total_bytes, 0);
    pcm
}

/// Create a 44-byte WAV init segment header (streaming mode: sizes = 0xFFFFFFFF).
#[must_use]
pub fn create_wav_init_header(sample_rate: u32, channels: u16) -> Vec<u8> {
    let bytes_per_sample: u16 = 2;
    let byte_rate = sample_rate * channels as u32 * bytes_per_sample as u32;
    let block_align = channels * bytes_per_sample;
    let data_size = 0xFFFF_FFFFu32;
    let file_size = 0xFFFF_FFFFu32;

    let mut wav = Vec::with_capacity(44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
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

    #[test]
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

    #[test]
    fn generate_segment_has_correct_prefix() {
        let data = generate_segment(1, 2, 100);
        assert!(data.starts_with(b"V1-SEG-2:TEST_SEGMENT_DATA"));
        assert_eq!(data.len(), 100);
        assert_eq!(data[99], 0xFF);
    }

    #[test]
    fn wav_init_header_is_44_bytes() {
        let header = create_wav_init_header(44100, 2);
        assert_eq!(header.len(), 44);
        assert!(header.starts_with(b"RIFF"));
    }
}
