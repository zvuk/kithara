//! Minimal HLS fixture helpers backed by unified `/stream/*` routes.

use std::{sync::Arc, time::Duration};

use url::Url;

use crate::{
    CreatedHls, HlsFixtureBuilder, HlsSpec, TestServerHelper,
    fixture_protocol::{DataMode, DelayRule, EncryptionRequest, InitMode},
};

/// Compatibility fixture that preserves historical byte-exact HLS payloads.
///
/// Prefer [`PackagedTestServer`] or `HlsFixtureBuilder::packaged_audio_*` for new
/// audio HLS tests.
pub struct TestServer {
    encrypted: CreatedHls,
    init: CreatedHls,
    plain: CreatedHls,
    _helper: TestServerHelper,
}

impl TestServer {
    #[must_use]
    pub async fn new() -> Self {
        let helper = TestServerHelper::new().await;
        let plain = helper
            .create_hls(HlsFixtureBuilder::from_spec(compat_fixed_plain_spec()))
            .await
            .expect("create fixed plain HLS fixture");
        let init = helper
            .create_hls(HlsFixtureBuilder::from_spec(compat_fixed_init_spec()))
            .await
            .expect("create fixed init HLS fixture");
        let encrypted = helper
            .create_hls(HlsFixtureBuilder::from_spec(compat_fixed_encrypted_spec()))
            .await
            .expect("create fixed encrypted HLS fixture");
        Self {
            plain,
            init,
            encrypted,
            _helper: helper,
        }
    }

    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        match path {
            "/master.m3u8" => self.plain.master_url(),
            "/master-init.m3u8" => self.init.master_url(),
            "/master-encrypted.m3u8" => self.encrypted.master_url(),
            "/v0.m3u8" => self.plain.media_url(0),
            "/v1.m3u8" | "/video/480p/playlist.m3u8" => self.plain.media_url(1),
            "/v2.m3u8" => self.plain.media_url(2),
            "/v0-init.m3u8" => self.init.media_url(0),
            "/v1-init.m3u8" => self.init.media_url(1),
            "/v2-init.m3u8" => self.init.media_url(2),
            "/v0-encrypted.m3u8" => self.encrypted.media_url(0),
            "/seg/v0_0.bin" => self.plain.segment_url(0, 0),
            "/seg/v0_1.bin" => self.plain.segment_url(0, 1),
            "/seg/v0_2.bin" => self.plain.segment_url(0, 2),
            "/seg/v1_0.bin" => self.plain.segment_url(1, 0),
            "/seg/v1_1.bin" => self.plain.segment_url(1, 1),
            "/seg/v1_2.bin" => self.plain.segment_url(1, 2),
            "/seg/v2_0.bin" => self.plain.segment_url(2, 0),
            "/seg/v2_1.bin" => self.plain.segment_url(2, 1),
            "/seg/v2_2.bin" => self.plain.segment_url(2, 2),
            "/init/v0.bin" => self.init.init_url(0),
            "/init/v1.bin" => self.init.init_url(1),
            "/init/v2.bin" => self.init.init_url(2),
            "/key.bin" => self.plain.key_url(),
            "/aes/key.bin" => self.encrypted.key_url(),
            "/aes/seg0.bin" => self.encrypted.segment_url(0, 0),
            "/custom/base/" | "/base/" => self._helper.url(path),
            other => panic!("unknown TestServer compatibility path `{other}`"),
        }
    }
}

#[crate::kithara::fixture]
pub async fn test_server() -> TestServer {
    TestServer::new().await
}

#[must_use]
pub fn test_master_playlist() -> &'static str {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=854x480,CODECS="avc1.42c01e,mp4a.40.2"
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1280x720,CODECS="avc1.42c01e,mp4a.40.2"
v1.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5120000,RESOLUTION=1920x1080,CODECS="avc1.42c01e,mp4a.40.2"
v2.m3u8
"#
}

#[must_use]
pub fn test_master_playlist_with_init() -> &'static str {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=854x480,CODECS="avc1.42c01e,mp4a.40.2"
v0-init.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1280x720,CODECS="avc1.42c01e,mp4a.40.2"
v1-init.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5120000,RESOLUTION=1920x1080,CODECS="avc1.42c01e,mp4a.40.2"
v2-init.m3u8
"#
}

#[must_use]
pub fn test_media_playlist(variant: usize) -> String {
    format!(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
seg/v{}_0.bin
#EXTINF:4.0,
seg/v{}_1.bin
#EXTINF:4.0,
seg/v{}_2.bin
#EXT-X-ENDLIST
"#,
        variant, variant, variant
    )
}

#[must_use]
pub fn test_media_playlist_with_init(variant: usize) -> String {
    format!(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MAP:URI="init/v{}.bin"
#EXTINF:4.0,
seg/v{}_0.bin
#EXTINF:4.0,
seg/v{}_1.bin
#EXTINF:4.0,
seg/v{}_2.bin
#EXT-X-ENDLIST
"#,
        variant, variant, variant, variant
    )
}

#[must_use]
pub fn test_segment_data(variant: usize, segment: usize) -> Vec<u8> {
    let prefix = format!("V{variant}-SEG-{segment}:");
    let mut data = prefix.into_bytes();
    data.extend(b"TEST_SEGMENT_DATA");
    if data.len() < 200_000 {
        data.resize(200_000, 0xFF);
    }
    data
}

#[must_use]
pub fn test_master_playlist_encrypted() -> &'static str {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=854x480,CODECS="avc1.42c01e,mp4a.40.2"
v0-encrypted.m3u8
"#
}

#[must_use]
pub fn test_media_playlist_encrypted(_variant: usize) -> String {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-KEY:METHOD=AES-128,URI="../aes/key.bin",IV=0x00000000000000000000000000000000
#EXTINF:4.0,
../aes/seg0.bin
#EXT-X-ENDLIST
"#
    .to_string()
}

/// AES-128 encryption configuration for configurable HLS fixtures.
pub struct EncryptionConfig {
    pub iv: Option<[u8; 16]>,
    pub key: [u8; 16],
}

/// Configuration for [`HlsTestServer`].
pub struct HlsTestServerConfig {
    pub custom_data: Option<Arc<Vec<u8>>>,
    pub custom_data_per_variant: Option<Vec<Arc<Vec<u8>>>>,
    pub encryption: Option<EncryptionConfig>,
    pub head_reported_segment_size: Option<usize>,
    pub init_data_per_variant: Option<Vec<Arc<Vec<u8>>>>,
    pub variant_bandwidths: Option<Vec<u64>>,
    pub delay_rules: Vec<DelayRule>,
    pub segment_duration_secs: f64,
    pub segment_size: usize,
    pub segments_per_variant: usize,
    pub variant_count: usize,
}

impl Default for HlsTestServerConfig {
    fn default() -> Self {
        Self {
            variant_count: 1,
            segments_per_variant: 3,
            segment_size: 200_000,
            segment_duration_secs: 4.0,
            custom_data: None,
            custom_data_per_variant: None,
            init_data_per_variant: None,
            variant_bandwidths: None,
            delay_rules: Vec::new(),
            encryption: None,
            head_reported_segment_size: None,
        }
    }
}

/// Configurable compatibility fixture for byte-exact synthetic HLS payloads.
///
/// This server keeps the historical test-pattern byte contract and therefore
/// remains the right choice for `expected_byte_at()`-style assertions.
pub struct HlsTestServer {
    created: CreatedHls,
    config: HlsTestServerConfig,
    _helper: TestServerHelper,
}

impl HlsTestServer {
    #[must_use]
    pub async fn new(config: HlsTestServerConfig) -> Self {
        let helper = TestServerHelper::new().await;
        let created = helper
            .create_hls(builder_from_config(&config))
            .await
            .expect("create configurable HLS fixture");
        Self {
            config,
            created,
            _helper: helper,
        }
    }

    #[must_use]
    pub fn config(&self) -> &HlsTestServerConfig {
        &self.config
    }

    #[must_use]
    pub fn expected_byte_at(&self, variant: usize, offset: u64) -> u8 {
        let init_len = self
            .config
            .init_data_per_variant
            .as_ref()
            .and_then(|d| d.get(variant))
            .map_or(0u64, |d| d.len() as u64);

        if offset < init_len {
            return self
                .config
                .init_data_per_variant
                .as_ref()
                .and_then(|data| data.get(variant))
                .and_then(|data| data.get(offset as usize))
                .copied()
                .unwrap_or(0);
        }

        let media_offset = offset - init_len;
        if let Some(ref per_variant) = self.config.custom_data_per_variant
            && let Some(data) = per_variant.get(variant)
        {
            return data.get(media_offset as usize).copied().unwrap_or(0);
        }
        if let Some(data) = &self.config.custom_data {
            return data.get(media_offset as usize).copied().unwrap_or(0);
        }

        let seg_idx = (media_offset / self.config.segment_size as u64) as usize;
        let off_in_seg = (media_offset % self.config.segment_size as u64) as usize;
        let prefix = format!("V{variant}-SEG-{seg_idx}:TEST_SEGMENT_DATA");
        let prefix_bytes = prefix.as_bytes();
        if off_in_seg < prefix_bytes.len() {
            prefix_bytes[off_in_seg]
        } else {
            0xFF
        }
    }

    #[must_use]
    pub fn init_len(&self) -> u64 {
        self.config
            .init_data_per_variant
            .as_ref()
            .and_then(|d| d.first())
            .map_or(0, |d| d.len() as u64)
    }

    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.init_len() + self.config.segments_per_variant as u64 * self.config.segment_size as u64
    }

    #[must_use]
    pub fn total_duration_secs(&self) -> f64 {
        self.config.segments_per_variant as f64 * self.config.segment_duration_secs
    }

    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        match path {
            "/master.m3u8" => self.created.master_url(),
            "/key.bin" => self.created.key_url(),
            other if other.starts_with("/playlist/v") && other.ends_with(".m3u8") => {
                let variant = parse_variant(other, "/playlist/v", ".m3u8")
                    // formatted message captures `other` via closure binding
                    // ast-grep-ignore: style.prefer-expect
                    .unwrap_or_else(|| panic!("invalid HlsTestServer playlist path `{other}`"));
                self.created.media_url(variant)
            }
            other if other.starts_with("/seg/v") && other.ends_with(".bin") => {
                let (variant, segment) = parse_segment(other)
                    // formatted message captures `other` via closure binding
                    // ast-grep-ignore: style.prefer-expect
                    .unwrap_or_else(|| panic!("invalid HlsTestServer segment path `{other}`"));
                self.created.segment_url(variant, segment)
            }
            other if other.starts_with("/init/v") && other.ends_with("_init.bin") => {
                let variant = parse_variant(other, "/init/v", "_init.bin")
                    // formatted message captures `other` via closure binding
                    // ast-grep-ignore: style.prefer-expect
                    .unwrap_or_else(|| panic!("invalid HlsTestServer init path `{other}`"));
                self.created.init_url(variant)
            }
            other => panic!("unknown HlsTestServer path `{other}`"),
        }
    }
}

/// Preferred packaged-audio fixture for new synthetic HLS audio tests.
pub struct PackagedTestServer {
    encrypted: CreatedHls,
    plain: CreatedHls,
    _helper: TestServerHelper,
}

impl PackagedTestServer {
    #[must_use]
    pub async fn new() -> Self {
        Self::with_delay_rules(Vec::new()).await
    }

    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        match path {
            "/master.m3u8" | "/master-init.m3u8" => self.plain.master_url(),
            "/master-encrypted.m3u8" => self.encrypted.master_url(),
            "/v0.m3u8" => self.plain.media_url(0),
            "/v1.m3u8" | "/video/480p/playlist.m3u8" => self.plain.media_url(1),
            "/v2.m3u8" => self.plain.media_url(2),
            "/v0-encrypted.m3u8" => self.encrypted.media_url(0),
            "/init/v0.mp4" => self.plain.init_url(0),
            "/init/v1.mp4" => self.plain.init_url(1),
            "/init/v2.mp4" => self.plain.init_url(2),
            "/seg/v0_0.m4s" => self.plain.segment_url(0, 0),
            "/seg/v0_1.m4s" => self.plain.segment_url(0, 1),
            "/seg/v0_2.m4s" => self.plain.segment_url(0, 2),
            "/seg/v1_0.m4s" => self.plain.segment_url(1, 0),
            "/seg/v1_1.m4s" => self.plain.segment_url(1, 1),
            "/seg/v1_2.m4s" => self.plain.segment_url(1, 2),
            "/seg/v2_0.m4s" => self.plain.segment_url(2, 0),
            "/seg/v2_1.m4s" => self.plain.segment_url(2, 1),
            "/seg/v2_2.m4s" => self.plain.segment_url(2, 2),
            "/key.bin" | "/aes/key.bin" => self.encrypted.key_url(),
            "/aes/seg0.m4s" => self.encrypted.segment_url(0, 0),
            other => panic!("unknown PackagedTestServer path `{other}`"),
        }
    }

    /// Build a server whose plain (3-variant AAC fMP4) fixture applies the
    /// given per-segment server-side delays. Lets tests pin behaviour
    /// under simulated slow connections — the encrypted fixture is built
    /// without delays as it is unaffected by these scenarios.
    #[must_use]
    pub async fn with_delay_rules(delay_rules: Vec<DelayRule>) -> Self {
        let helper = TestServerHelper::new().await;
        let plain = helper
            .create_hls(packaged_plain_builder().delay_rules(delay_rules))
            .await
            .expect("create packaged plain HLS fixture");
        let encrypted = helper
            .create_hls(packaged_encrypted_builder())
            .await
            .expect("create packaged encrypted HLS fixture");
        Self {
            plain,
            encrypted,
            _helper: helper,
        }
    }
}

#[crate::kithara::fixture]
pub async fn packaged_test_server() -> PackagedTestServer {
    PackagedTestServer::new().await
}

/// ABR-specific helpers preserved for existing tests.
pub mod abr {
    pub use super::{AbrTestServer, master_playlist};
}

/// Compatibility re-exports for legacy byte-exact fixtures.
pub mod compat {
    pub use super::{
        AbrTestServer, HlsTestServer, TestServer, abr, master_playlist, test_master_playlist,
        test_master_playlist_encrypted, test_master_playlist_with_init, test_media_playlist,
        test_media_playlist_encrypted, test_media_playlist_with_init, test_segment_data,
        test_server,
    };
}

/// Re-exports for packaged fMP4 fixture presets.
pub mod packaged {
    pub use super::{PackagedTestServer, packaged_test_server};
}

/// Compatibility-only ABR fixture backed by the unified synthetic stream routes.
pub struct AbrTestServer {
    created: CreatedHls,
    _helper: TestServerHelper,
}

impl AbrTestServer {
    #[must_use]
    pub async fn new(master_playlist: String, init: bool, segment0_delay: Duration) -> Self {
        let helper = TestServerHelper::new().await;
        let created = helper
            .create_hls(HlsFixtureBuilder::from_spec(compat_abr_spec(
                &master_playlist,
                init,
                segment0_delay,
            )))
            .await
            .expect("create ABR HLS fixture");
        Self {
            created,
            _helper: helper,
        }
    }

    #[must_use]
    pub fn url(&self, path: &str) -> Url {
        match path {
            "/master.m3u8" => self.created.master_url(),
            "/v0.m3u8" => self.created.media_url(0),
            "/v1.m3u8" => self.created.media_url(1),
            "/v2.m3u8" => self.created.media_url(2),
            "/seg/v0_0.bin" => self.created.segment_url(0, 0),
            "/seg/v0_1.bin" => self.created.segment_url(0, 1),
            "/seg/v0_2.bin" => self.created.segment_url(0, 2),
            "/seg/v1_0.bin" => self.created.segment_url(1, 0),
            "/seg/v1_1.bin" => self.created.segment_url(1, 1),
            "/seg/v1_2.bin" => self.created.segment_url(1, 2),
            "/seg/v2_0.bin" => self.created.segment_url(2, 0),
            "/seg/v2_1.bin" => self.created.segment_url(2, 1),
            "/seg/v2_2.bin" => self.created.segment_url(2, 2),
            "/init/v0.bin" => self.created.init_url(0),
            "/init/v1.bin" => self.created.init_url(1),
            "/init/v2.bin" => self.created.init_url(2),
            other => panic!("unknown AbrTestServer path `{other}`"),
        }
    }
}

#[must_use]
pub fn master_playlist(v0_bw: u64, v1_bw: u64, v2_bw: u64) -> String {
    format!(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH={v0_bw}
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH={v1_bw}
v1.m3u8
#EXT-X-STREAM-INF:BANDWIDTH={v2_bw}
v2.m3u8
"#
    )
}

fn compat_fixed_plain_spec() -> HlsSpec {
    HlsSpec {
        variant_count: 3,
        key_hex: Some(hex::encode(test_key_data())),
        ..HlsSpec::default()
    }
}

fn compat_fixed_init_spec() -> HlsSpec {
    HlsSpec {
        variant_count: 3,
        init_mode: InitMode::TestInit,
        ..compat_fixed_plain_spec()
    }
}

fn compat_fixed_encrypted_spec() -> HlsSpec {
    HlsSpec {
        variant_count: 1,
        segments_per_variant: 1,
        segment_size: aes128_plaintext_segment().len(),
        data_mode: DataMode::CustomDataPerVariant(vec![aes128_plaintext_segment()]),
        encryption: Some(EncryptionRequest {
            key_hex: hex::encode(aes128_key_bytes()),
            iv_hex: Some(hex::encode(aes128_iv())),
        }),
        head_reported_segment_size: Some(aes128_plaintext_segment().len()),
        ..HlsSpec::default()
    }
}

fn packaged_plain_builder() -> HlsFixtureBuilder {
    HlsFixtureBuilder::new()
        .variant_count(3)
        .segments_per_variant(3)
        .segment_duration_secs(4.0)
        .variant_bandwidths(vec![1_280_000, 2_560_000, 5_120_000])
        .packaged_audio_aac_lc(44_100, 2)
}

fn packaged_encrypted_builder() -> HlsFixtureBuilder {
    packaged_plain_builder()
        .variant_count(1)
        .variant_bandwidths(vec![1_280_000])
        .encryption(EncryptionRequest {
            key_hex: hex::encode(aes128_key_bytes()),
            iv_hex: Some(hex::encode(aes128_iv())),
        })
}

fn builder_from_config(config: &HlsTestServerConfig) -> HlsFixtureBuilder {
    let builder = HlsFixtureBuilder::new()
        .variant_count(config.variant_count)
        .segments_per_variant(config.segments_per_variant)
        .segment_size(config.segment_size)
        .segment_duration_secs(config.segment_duration_secs)
        .delay_rules(config.delay_rules.clone());
    let builder = if let Some(variant_bandwidths) = config.variant_bandwidths.clone() {
        builder.variant_bandwidths(variant_bandwidths)
    } else {
        builder
    };
    let builder = if let Some(encryption) = config.encryption.as_ref() {
        builder.encryption(EncryptionRequest {
            key_hex: hex::encode(encryption.key),
            iv_hex: encryption.iv.map(hex::encode),
        })
    } else {
        builder
    };
    let builder = if let Some(head_reported_segment_size) = config
        .head_reported_segment_size
        .or_else(|| config.encryption.as_ref().map(|_| config.segment_size))
    {
        builder.head_reported_segment_size(head_reported_segment_size)
    } else {
        builder
    };
    let builder = if let Some(data) = config.custom_data.as_ref() {
        builder.custom_data(Arc::clone(data))
    } else {
        builder
    };
    let builder = if let Some(data) = config.custom_data_per_variant.as_ref() {
        builder.custom_data_per_variant(data.clone())
    } else {
        builder
    };
    if let Some(data) = config.init_data_per_variant.as_ref() {
        builder.init_data_per_variant(data.clone())
    } else {
        builder
    }
}

fn compat_abr_spec(master_playlist: &str, init: bool, segment0_delay: Duration) -> HlsSpec {
    let variant_bandwidths = parse_master_bandwidths(master_playlist);
    let init_mode = if init {
        InitMode::Custom((0..3).map(compat_abr_init_data).collect())
    } else {
        InitMode::None
    };

    HlsSpec {
        variant_count: variant_bandwidths.len().max(1),
        data_mode: DataMode::AbrBinary,
        init_mode,
        variant_bandwidths: Some(variant_bandwidths),
        delay_rules: vec![DelayRule {
            variant: Some(2),
            segment_eq: Some(0),
            segment_gte: None,
            delay_ms: segment0_delay.as_millis() as u64,
        }],
        head_reported_segment_size: Some(200_000),
        ..HlsSpec::default()
    }
}

fn parse_master_bandwidths(master_playlist: &str) -> Vec<u64> {
    master_playlist
        .lines()
        .filter_map(|line| line.strip_prefix("#EXT-X-STREAM-INF:BANDWIDTH="))
        .filter_map(|value| value.split(',').next())
        .filter_map(|value| value.parse::<u64>().ok())
        .collect()
}

fn parse_variant(path: &str, prefix: &str, suffix: &str) -> Option<usize> {
    path.strip_prefix(prefix)?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

fn parse_segment(path: &str) -> Option<(usize, usize)> {
    let path = path.strip_prefix("/seg/v")?.strip_suffix(".bin")?;
    let (variant, segment) = path.split_once('_')?;
    Some((variant.parse().ok()?, segment.parse().ok()?))
}

fn compat_abr_init_data(variant: usize) -> Vec<u8> {
    format!("V{variant}-INIT:").into_bytes()
}

fn aes128_key_bytes() -> Vec<u8> {
    b"0123456789abcdef".to_vec()
}

fn aes128_iv() -> [u8; 16] {
    [0u8; 16]
}

fn aes128_plaintext_segment() -> Vec<u8> {
    b"V0-SEG-0:DRM-PLAINTEXT".to_vec()
}

fn test_key_data() -> Vec<u8> {
    b"TEST_KEY_DATA_123456".to_vec()
}
