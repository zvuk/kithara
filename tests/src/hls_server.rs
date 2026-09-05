use std::iter;

use kithara::{
    platform::{sync::Arc, time::Duration},
    stream::AudioCodec,
};
use url::Url;

use crate::{
    CreatedHls, HlsFixtureBuilder, HlsSpec, TestServerHelper,
    fixture_protocol::{DataMode, DelayRule, EncryptionRequest, HttpErrorRule, InitMode},
};

macro_rules! url_method {
    ($self:ident, $path:ident, $message:literal, $resolve:expr) => {
        #[must_use]
        pub fn url(&$self, $path: &str) -> Url {
            $resolve.unwrap_or_else(|| panic!(concat!($message, " `{}`"), $path))
        }
    };
}

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

    url_method!(
        self,
        path,
        "unknown TestServer compatibility path",
        route_hls_path(
            &self.plain,
            &self.init,
            path,
            ".bin",
            ".bin",
            |plain, path| {
                match path {
                    "/master-init.m3u8" => Some(self.init.master_url()),
                    "/v0-init.m3u8" => Some(self.init.media_url(0)),
                    "/v1-init.m3u8" => Some(self.init.media_url(1)),
                    "/v2-init.m3u8" => Some(self.init.media_url(2)),
                    "/key.bin" => Some(plain.key_url()),
                    "/aes/seg0.bin" => Some(self.encrypted.segment_url(0, 0)),
                    "/custom/base/" | "/base/" => Some(self._helper.url(path)),
                    _ => None,
                }
                .or_else(|| route_compat_alias(plain, &self.encrypted, path))
                .or_else(|| route_media_playlist_path(plain, path))
            },
        )
    );
}

#[::kithara::fixture]
pub async fn test_server() -> TestServer {
    TestServer::new().await
}

#[must_use]
pub const fn test_master_playlist() -> &'static str {
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
pub const fn test_master_playlist_with_init() -> &'static str {
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
pub const fn test_master_playlist_encrypted() -> &'static str {
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
    /// Optional `CODECS` attribute emitted on every `EXT-X-STREAM-INF`
    /// in the master playlist. Lets fixtures with synthetic payloads
    /// (raw PCM under `.m4s` URIs) signal the real container so the
    /// HLS coordinator routes ABR switches through the right path
    /// (e.g. `"wav"` keeps the existing decoder for byte-continuity
    /// containers; `Fmp4` is otherwise inferred from URI extension).
    pub codecs: Option<String>,
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
            codecs: None,
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

    /// Build a configurable HLS server that withholds the GET (body)
    /// response for one `(variant, segment)` until the returned handle
    /// releases it. The HEAD (size) response stays unblocked, so the
    /// up-front size-estimation pass still learns the segment size while
    /// the body is parked.
    ///
    /// Deterministic, network-free, timer-free seam for "this segment has
    /// not arrived yet" scenarios over the WAV/`HlsTestServer` fixture path
    /// (the AAC fMP4 analogue is [`PackagedTestServer::with_segment_gate`]).
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub async fn with_segment_gate(
        config: HlsTestServerConfig,
        variant: usize,
        segment: usize,
    ) -> (Self, crate::SegmentGateHandle) {
        let server = Self::new(config).await;
        let handle = server
            ._helper
            .register_segment_gate(server.created.token(), variant, segment);
        (server, handle)
    }

    /// Withhold one variant's init (`EXT-X-MAP`) segment until released.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn init_gate(&self, variant: usize) -> crate::InitGateHandle {
        self._helper
            .register_init_gate(self.created.token(), variant)
    }

    #[must_use]
    pub const fn config(&self) -> &HlsTestServerConfig {
        &self.config
    }

    /// Size-probes (`HEAD` + single-byte ranged `GET`) the server has served
    /// for one `(variant, segment)` of this fixture.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn size_probe_count(&self, variant: usize, segment: usize) -> u64 {
        self._helper
            .size_probe_count(self.created.token(), variant, segment)
    }

    /// Total size-probes served across every segment of one variant.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn variant_size_probe_count(&self, variant: usize) -> u64 {
        (0..self.config.segments_per_variant)
            .map(|segment| self.size_probe_count(variant, segment))
            .sum()
    }

    /// Total size-probes served across every segment of every variant.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn total_size_probe_count(&self) -> u64 {
        (0..self.config.variant_count)
            .map(|variant| self.variant_size_probe_count(variant))
            .sum()
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

    pub fn for_each_expected_byte_mismatch(
        &self,
        variant: usize,
        offset: u64,
        actual: &[u8],
        mut mismatch: impl FnMut(usize, u8, u8),
    ) {
        let init_data = self
            .config
            .init_data_per_variant
            .as_ref()
            .and_then(|data| data.get(variant));
        let init_len = init_data.map_or(0u64, |data| data.len() as u64);

        let mut checked = 0usize;
        if offset < init_len {
            let init_count = match usize::try_from(init_len - offset) {
                Ok(remaining) => actual.len().min(remaining),
                Err(_) => actual.len(),
            };
            if let Some(data) = init_data {
                Self::scan_data_mismatches(data, offset, &actual[..init_count], 0, &mut mismatch);
            } else {
                Self::scan_mismatches(iter::repeat(0), &actual[..init_count], 0, &mut mismatch);
            }
            checked = init_count;
        }

        if checked == actual.len() {
            return;
        }

        let media_offset = offset.saturating_sub(init_len);
        let media_actual = &actual[checked..];
        if let Some(ref per_variant) = self.config.custom_data_per_variant
            && let Some(data) = per_variant.get(variant)
        {
            Self::scan_data_mismatches(data, media_offset, media_actual, checked, &mut mismatch);
            return;
        }
        if let Some(data) = &self.config.custom_data {
            Self::scan_data_mismatches(data, media_offset, media_actual, checked, &mut mismatch);
            return;
        }

        self.scan_generated_mismatches(variant, media_offset, media_actual, checked, &mut mismatch);
    }

    fn scan_data_mismatches(
        data: &[u8],
        data_offset: u64,
        actual: &[u8],
        base_index: usize,
        mismatch: &mut impl FnMut(usize, u8, u8),
    ) {
        let Ok(data_offset) = usize::try_from(data_offset) else {
            Self::scan_mismatches(iter::repeat(0), actual, base_index, mismatch);
            return;
        };

        if data_offset >= data.len() {
            Self::scan_mismatches(iter::repeat(0), actual, base_index, mismatch);
            return;
        }

        let data_len = actual.len().min(data.len() - data_offset);
        Self::scan_mismatches(
            data[data_offset..data_offset + data_len].iter().copied(),
            &actual[..data_len],
            base_index,
            mismatch,
        );
        if data_len < actual.len() {
            Self::scan_mismatches(
                iter::repeat(0),
                &actual[data_len..],
                base_index + data_len,
                mismatch,
            );
        }
    }

    fn scan_generated_mismatches(
        &self,
        variant: usize,
        mut media_offset: u64,
        actual: &[u8],
        base_index: usize,
        mismatch: &mut impl FnMut(usize, u8, u8),
    ) {
        let segment_size = self.config.segment_size as u64;
        let mut checked = 0usize;
        while checked < actual.len() {
            let seg_idx = (media_offset / segment_size) as usize;
            let off_in_seg = (media_offset % segment_size) as usize;
            let remaining_in_segment = self.config.segment_size - off_in_seg;
            let n = remaining_in_segment.min(actual.len() - checked);
            let prefix = format!("V{variant}-SEG-{seg_idx}:TEST_SEGMENT_DATA");
            let prefix_bytes = prefix.as_bytes();

            if off_in_seg < prefix_bytes.len() {
                let prefix_len = n.min(prefix_bytes.len() - off_in_seg);
                Self::scan_mismatches(
                    prefix_bytes[off_in_seg..off_in_seg + prefix_len]
                        .iter()
                        .copied(),
                    &actual[checked..checked + prefix_len],
                    base_index + checked,
                    mismatch,
                );
                if prefix_len < n {
                    Self::scan_mismatches(
                        iter::repeat(0xFF),
                        &actual[checked + prefix_len..checked + n],
                        base_index + checked + prefix_len,
                        mismatch,
                    );
                }
            } else {
                Self::scan_mismatches(
                    iter::repeat(0xFF),
                    &actual[checked..checked + n],
                    base_index + checked,
                    mismatch,
                );
            }

            checked += n;
            media_offset += n as u64;
        }
    }

    fn scan_mismatches(
        expected: impl IntoIterator<Item = u8>,
        actual: &[u8],
        base_index: usize,
        mismatch: &mut impl FnMut(usize, u8, u8),
    ) {
        for (index, (expected_byte, &actual_byte)) in expected.into_iter().zip(actual).enumerate() {
            if actual_byte != expected_byte {
                mismatch(base_index + index, expected_byte, actual_byte);
            }
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

    url_method!(
        self,
        path,
        "unknown HlsTestServer path",
        route_hls_path(
            &self.created,
            &self.created,
            path,
            ".bin",
            "_init.bin",
            |fixture, path| {
                if path == "/key.bin" {
                    return Some(fixture.key_url());
                }
                let variant = path
                    .strip_prefix("/playlist/v")?
                    .strip_suffix(".m3u8")?
                    .parse()
                    .expect("invalid HlsTestServer playlist path");
                Some(fixture.media_url(variant))
            },
        )
    );
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
        Self::from_builders(packaged_plain_builder(), packaged_encrypted_builder()).await
    }

    url_method!(
        self,
        path,
        "unknown PackagedTestServer path",
        route_hls_path(
            &self.plain,
            &self.plain,
            path,
            ".m4s",
            ".mp4",
            |plain, path| {
                match path {
                    "/master-init.m3u8" => Some(plain.master_url()),
                    "/key.bin" => Some(self.encrypted.key_url()),
                    "/aes/seg0.m4s" => Some(self.encrypted.segment_url(0, 0)),
                    _ => None,
                }
                .or_else(|| route_compat_alias(plain, &self.encrypted, path))
                .or_else(|| route_media_playlist_path(plain, path))
            },
        )
    );

    /// Build a server whose plain (3-variant AAC fMP4) fixture applies the
    /// given per-segment server-side delays. Lets tests pin behaviour
    /// under simulated slow connections — the encrypted fixture is built
    /// without delays as it is unaffected by these scenarios.
    #[must_use]
    pub async fn with_delay_rules(delay_rules: Vec<DelayRule>) -> Self {
        Self::from_builders(
            packaged_plain_builder().delay_rules(delay_rules),
            packaged_encrypted_builder(),
        )
        .await
    }

    /// Build a server whose encrypted fixture applies the given HTTP
    /// error rules (e.g. 403 on the key endpoint). The plain fixture is
    /// built without error rules and stays usable for unaffected tests.
    #[must_use]
    pub async fn with_error_rules(error_rules: Vec<HttpErrorRule>) -> Self {
        Self::from_builders(
            packaged_plain_builder(),
            packaged_encrypted_builder().error_rules(error_rules),
        )
        .await
    }

    async fn from_builders(plain: HlsFixtureBuilder, encrypted: HlsFixtureBuilder) -> Self {
        let helper = TestServerHelper::new().await;
        let plain = helper
            .create_hls(plain)
            .await
            .expect("create packaged plain HLS fixture");
        let encrypted = helper
            .create_hls(encrypted)
            .await
            .expect("create packaged encrypted HLS fixture");
        Self {
            plain,
            encrypted,
            _helper: helper,
        }
    }

    /// Build a packaged server whose `plain` (3-variant AAC fMP4) fixture
    /// withholds the GET response for one `(variant, segment)` until the
    /// returned handle releases it. The HEAD (size) response stays unblocked,
    /// so the consumer still learns the segment size while the body is parked.
    ///
    /// This is the deterministic, network-free, timer-free seam for
    /// "segment not yet loaded" / early-seek scenarios: a test seeks into the
    /// withheld segment, asserts the reader makes no false progress while
    /// withheld, then `release()`s and asserts playback resumes.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub async fn with_segment_gate(
        variant: usize,
        segment: usize,
    ) -> (Self, crate::SegmentGateHandle) {
        let (server, mut handles) = Self::with_segment_gates(&[(variant, segment)]).await;
        let handle = handles.pop().expect("one segment gate handle");
        (server, handle)
    }

    /// Build a packaged server whose `plain` fixture withholds each listed
    /// `(variant, segment)` GET body until its corresponding handle releases it.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub async fn with_segment_gates(
        gates: &[(usize, usize)],
    ) -> (Self, Vec<crate::SegmentGateHandle>) {
        let server = Self::new().await;
        let handles = gates
            .iter()
            .map(|&(variant, segment)| {
                server
                    ._helper
                    .register_segment_gate(server.plain.token(), variant, segment)
            })
            .collect();
        (server, handles)
    }

    /// Build a packaged server whose `plain` fixture withholds the **size**
    /// (HEAD) of one `(variant, segment)` from construction: the up-front
    /// `loading::size_estimation` pass learns `Content-Length: 0` for it, so
    /// the segment's bytes are unaccounted-for in `total_bytes`. Models a seek
    /// that lands BEFORE the segment's size is known — the genuinely-immediate
    /// seek the body-only [`Self::with_segment_gate`] cannot model (a body
    /// withhold would instead block `PlayWorker::open`, since estimation HEADs
    /// every segment synchronously at construction).
    ///
    /// The body GET stays unblocked. The returned handle can `release_head()`
    /// to reveal the true size, and `withhold_head()` / `release()` give
    /// independent control of the size and body seams respectively.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub async fn with_segment_size_gate(
        variant: usize,
        segment: usize,
    ) -> (Self, crate::SegmentGateHandle) {
        let server = Self::new().await;
        let handle = server
            ._helper
            .register_segment_gate(server.plain.token(), variant, segment);
        handle.withhold_head();
        // A registered gate parks the body GET by default; this constructor
        // withholds only the size, so open the body up front.
        handle.release();
        (server, handle)
    }
}

#[::kithara::fixture]
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
        test_master_playlist_encrypted, test_master_playlist_with_init,
        test_media_playlist_encrypted, test_segment_data, test_server,
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

    url_method!(
        self,
        path,
        "unknown AbrTestServer path",
        route_hls_path(
            &self.created,
            &self.created,
            path,
            ".bin",
            ".bin",
            route_media_playlist_path,
        )
    );
}

fn route_compat_alias(plain: &CreatedHls, encrypted: &CreatedHls, path: &str) -> Option<Url> {
    match path {
        "/master-encrypted.m3u8" => Some(encrypted.master_url()),
        "/video/480p/playlist.m3u8" => Some(plain.media_url(1)),
        "/v0-encrypted.m3u8" => Some(encrypted.media_url(0)),
        "/aes/key.bin" => Some(encrypted.key_url()),
        _ => None,
    }
}

fn route_hls_path(
    fixture: &CreatedHls,
    init: &CreatedHls,
    path: &str,
    segment_ext: &str,
    init_ext: &str,
    alias: impl FnOnce(&CreatedHls, &str) -> Option<Url>,
) -> Option<Url> {
    (path == "/master.m3u8")
        .then(|| fixture.master_url())
        .or_else(|| alias(fixture, path))
        .or_else(|| route_segment_path(fixture, path, "/seg/", segment_ext))
        .or_else(|| route_init_path(init, path, "/init/", init_ext))
}

/// Resolve a `/<prefix>vX_Y<ext>` segment path on the given fixture.
/// Returns `None` for non-matching paths so callers can fall through to
/// their fixture-specific routes.
fn route_segment_path(fixture: &CreatedHls, path: &str, prefix: &str, ext: &str) -> Option<Url> {
    let body = path.strip_prefix(prefix)?.strip_suffix(ext)?;
    let (variant_part, segment_part) = body.split_once('_')?;
    let variant: usize = variant_part.strip_prefix('v')?.parse().ok()?;
    let segment: usize = segment_part.parse().ok()?;
    Some(fixture.segment_url(variant, segment))
}

/// Resolve a `/<prefix>vX<ext>` init path on the given fixture.
fn route_init_path(fixture: &CreatedHls, path: &str, prefix: &str, ext: &str) -> Option<Url> {
    let variant_part = path.strip_prefix(prefix)?.strip_suffix(ext)?;
    let variant: usize = variant_part.strip_prefix('v')?.parse().ok()?;
    Some(fixture.init_url(variant))
}

/// Resolve a `/vX.m3u8` media-playlist path on the given fixture.
fn route_media_playlist_path(fixture: &CreatedHls, path: &str) -> Option<Url> {
    let variant_part = path.strip_prefix("/v")?.strip_suffix(".m3u8")?;
    let variant: usize = variant_part.parse().ok()?;
    Some(fixture.media_url(variant))
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

/// Ladder mirroring the production master playlist: three AAC-LC variants
/// under one FLAC lossless, at the bandwidths the real master advertises,
/// over 37 × 6 s.
///
/// Both halves are load-bearing and neither is optional for the tests that
/// take this ladder. The FLAC variant is the far side of the codec boundary
/// an ABR move has to cross; the length is what lets a test seek deep into
/// the track. `PackagedTestServer` stays the cheaper choice for anything
/// that needs neither.
#[must_use]
pub fn mixed_codec_ladder() -> HlsFixtureBuilder {
    HlsFixtureBuilder::new()
        .variant_count(4)
        .segments_per_variant(37)
        .segment_duration_secs(6.0)
        .variant_bandwidths(vec![66_005, 134_107, 269_930, 988_758])
        .packaged_audio_aac_lc(44_100, 2)
        .override_variant_codec(3, AudioCodec::Flac)
}

/// [`mixed_codec_ladder`] with AES-128 segments. The production DRM master
/// advertises the same four variants at the same bandwidths, so the two
/// differ only by encryption.
#[must_use]
pub fn mixed_codec_ladder_encrypted() -> HlsFixtureBuilder {
    mixed_codec_ladder().encryption(EncryptionRequest {
        key_hex: hex::encode(aes128_key_bytes()),
        iv_hex: Some(hex::encode(aes128_iv())),
    })
}

/// Serves [`mixed_codec_ladder`], plain or encrypted, and hands back its
/// master URL.
///
/// The two trees this ladder replaces differed only by encryption, so the
/// suites that read both of them take the axis as a flag rather than as two
/// separate setup paths.
pub async fn mixed_codec_ladder_url(server: &TestServerHelper, encrypted: bool) -> Url {
    let ladder = if encrypted {
        mixed_codec_ladder_encrypted()
    } else {
        mixed_codec_ladder()
    };
    server
        .create_hls(ladder)
        .await
        .expect("create the mixed-codec ladder")
        .master_url()
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
    let builder = if let Some(data) = config.init_data_per_variant.as_ref() {
        builder.init_data_per_variant(data.clone())
    } else {
        builder
    };
    if let Some(codecs) = config.codecs.as_ref() {
        builder.codecs(codecs.clone())
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

fn compat_abr_init_data(variant: usize) -> Vec<u8> {
    format!("V{variant}-INIT:").into_bytes()
}

fn aes128_key_bytes() -> Vec<u8> {
    b"0123456789abcdef".to_vec()
}

const fn aes128_iv() -> [u8; 16] {
    [0u8; 16]
}

fn aes128_plaintext_segment() -> Vec<u8> {
    b"V0-SEG-0:DRM-PLAINTEXT".to_vec()
}

fn test_key_data() -> Vec<u8> {
    b"TEST_KEY_DATA_123456".to_vec()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara::platform::{flash::real_io, time::Duration, tokio};

    use super::PackagedTestServer;
    use crate::kithara;

    /// The withhold gate parks a segment GET at the server until the test
    /// releases it, and the in-process `requested()` counter lets the test
    /// observe that the GET actually arrived. Polling that counter under a
    /// hard timeout is synchronization on an observable — a real stall fails
    /// the budget rather than being masked.
    //
    // This drives raw `reqwest` through the platform spawn chokepoint against
    // the shared test server (a real-time island, no flash ambient). The in-flight
    // request and its response live on the REAL clock, invisible to the flash
    // engine (no downloader `real_io` bracket wraps them). Hold a `RealIoScope`
    // across every wait on that real transit — the gated GET reaching the server
    // and the released GET completing — so the virtual clock is PACED to real
    // time while held: each `time::timeout` budget then fires only after the
    // equivalent REAL time, never spuriously ahead of bytes still on the wire.
    // The budget is preserved as a real stall oracle (a genuinely stuck request
    // still exhausts the paced 5s), not relaxed. Off the `flash` feature the
    // scope is a ZST no-op and the clock is already real.
    #[kithara::test(tokio)]
    async fn segment_gate_withholds_get_until_release() {
        let (server, gate) = PackagedTestServer::with_segment_gate(0, 1).await;
        let url = server.url("/seg/v0_1.m4s");
        assert_eq!(gate.requested(), 0, "no GET before the test fires one");

        let _real_io = real_io();

        let fetch = tokio::task::spawn(async move { reqwest::get(url).await.map(|r| r.status()) });

        time::timeout(Duration::from_secs(5), async {
            while gate.requested() == 0 {
                time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("withheld segment GET should reach the server within budget");

        assert!(
            !fetch.is_finished(),
            "withheld segment GET must not complete before release"
        );

        gate.release();

        let status = time::timeout(Duration::from_secs(5), fetch)
            .await
            .expect("released GET should complete within budget")
            .expect("segment GET task joins")
            .expect("segment GET succeeds");
        assert!(
            status.is_success() || status.as_u16() == 206,
            "released segment GET should succeed, got {status}"
        );
        assert_eq!(gate.requested(), 1, "exactly one GET parked on the gate");
    }

    /// The size/HEAD gate makes HEAD report `Content-Length: 0` while
    /// withheld (and counts the HEAD), then the true size after
    /// `release_head()` — without ever parking the HEAD (which would block
    /// stream construction). The body GET stays unblocked throughout.
    //
    // Same raw-reqwest-over-real-socket shape as above: hold a `RealIoScope`
    // across every real HEAD/GET so the virtual clock is paced to real time and
    // no `time::timeout` budget collapses ahead of the in-flight response. The
    // budget stays a real stall oracle, not relaxed (see the sibling test).
    #[kithara::test(tokio)]
    async fn segment_size_gate_reports_zero_until_release_head() {
        let (server, gate) = PackagedTestServer::with_segment_size_gate(0, 1).await;
        let url = server.url("/seg/v0_1.m4s");
        assert_eq!(
            gate.head_requested(),
            0,
            "no HEAD before the test fires one"
        );

        let _real_io = real_io();

        let withheld = time::timeout(
            Duration::from_secs(5),
            reqwest::Client::new().head(url.clone()).send(),
        )
        .await
        .expect("HEAD must not park while size is withheld")
        .expect("withheld-size HEAD succeeds");
        assert!(withheld.status().is_success());
        assert_eq!(
            withheld
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok()),
            Some("0"),
            "withheld size must be reported as Content-Length: 0"
        );
        assert_eq!(gate.head_requested(), 1, "HEAD reached the gate");

        gate.release_head();
        let revealed = time::timeout(
            Duration::from_secs(5),
            reqwest::Client::new().head(url).send(),
        )
        .await
        .expect("HEAD completes")
        .expect("revealed-size HEAD succeeds");
        let revealed_len: u64 = revealed
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .expect("revealed HEAD carries a content-length");
        assert!(
            revealed_len > 0,
            "released size must report the true (non-zero) length, got {revealed_len}"
        );

        // The body GET is independent of the size gate and stays unblocked.
        let body = time::timeout(
            Duration::from_secs(5),
            reqwest::get(server.url("/seg/v0_1.m4s")),
        )
        .await
        .expect("body GET completes (never parked by the size gate)")
        .expect("body GET succeeds");
        assert!(body.status().is_success() || body.status().as_u16() == 206);
    }
}
