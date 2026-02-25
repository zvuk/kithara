//! Configurable HLS test server with dynamic routing.
//!
//! Serves any number of variants and segments with deterministic data.
//! Segment data format: `V{variant}-SEG-{segment}:TEST_SEGMENT_DATA` + `0xFF` padding.
//!
//! On native: in-process axum server. On WASM: delegates to external fixture server.
//!
//! # Example
//!
//! ```ignore
//! let server = HlsTestServer::new(HlsTestServerConfig {
//!     segments_per_variant: 100,
//!     ..Default::default()
//! }).await;
//!
//! let url = server.url("/master.m3u8")?;
//! assert_eq!(server.total_bytes(), 20_000_000);
//! assert_eq!(server.expected_byte_at(0, 0), b'V');
//! ```

use std::sync::Arc;

use kithara_test_utils::fixture_protocol::DelayRule;

// ── Config (cross-platform) ────────────────────────────────────────

/// AES-128 encryption configuration for test server.
pub(crate) struct EncryptionConfig {
    /// 16-byte AES key.
    pub(crate) key: [u8; 16],
    /// 16-byte IV. When `None`, derived from segment sequence (standard HLS).
    pub(crate) iv: Option<[u8; 16]>,
}

/// Configuration for [`HlsTestServer`].
pub(crate) struct HlsTestServerConfig {
    /// Number of HLS variants (bitrate levels). Default: 1.
    pub(crate) variant_count: usize,
    /// Number of segments per variant. Default: 3.
    pub(crate) segments_per_variant: usize,
    /// Segment size in bytes. Default: 200 000 (200 KB).
    pub(crate) segment_size: usize,
    /// Segment duration in seconds (for playlist `#EXTINF`). Default: 4.0.
    pub(crate) segment_duration_secs: f64,
    /// Custom binary data to serve instead of generated test patterns.
    ///
    /// When set, segments are sliced from this data: segment N serves
    /// `data[N * segment_size .. (N+1) * segment_size]`.
    /// Total length must equal `segments_per_variant * segment_size`.
    pub(crate) custom_data: Option<Arc<Vec<u8>>>,
    /// Per-variant binary data for media segments.
    ///
    /// Segment N of variant V serves:
    /// `data[V][N * segment_size .. (N+1) * segment_size]`.
    /// Takes priority over `custom_data` when set.
    pub(crate) custom_data_per_variant: Option<Vec<Arc<Vec<u8>>>>,
    /// Per-variant init segment data (served via `#EXT-X-MAP` route).
    ///
    /// When set, playlists include `#EXT-X-MAP:URI="../init/v{variant}_init.bin"`.
    pub(crate) init_data_per_variant: Option<Vec<Arc<Vec<u8>>>>,
    /// Custom bandwidths for master playlist variants.
    ///
    /// When `None`, uses default `(v + 1) * 1_280_000`.
    pub(crate) variant_bandwidths: Option<Vec<u64>>,
    /// Declarative delay rules: first matching rule determines the sleep before serving.
    ///
    /// Used to simulate slow segments and trigger ABR switches.
    pub(crate) delay_rules: Vec<DelayRule>,
    /// AES-128 encryption config. When set, segments are served encrypted
    /// and playlists include `#EXT-X-KEY` tag.
    pub(crate) encryption: Option<EncryptionConfig>,
    /// Size reported by HEAD responses for segments (simulates compressed size).
    /// When set, HEAD returns this value as Content-Length instead of `segment_size`.
    pub(crate) head_reported_segment_size: Option<usize>,
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

// ── Shared verification logic (cross-platform) ─────────────────────

/// Compute expected byte for verification (shared logic for server + tests).
///
/// When init data is present, the byte stream layout is:
/// `[init_data][media_seg_0][media_seg_1]...[media_seg_N]`
/// Verification works with plaintext — decryption happens on the client side.
fn expected_byte_at_impl(config: &HlsTestServerConfig, variant: usize, offset: u64) -> u8 {
    // Init data region
    let init_len = config
        .init_data_per_variant
        .as_ref()
        .and_then(|d| d.get(variant))
        .map_or(0u64, |d| d.len() as u64);

    if offset < init_len {
        return config.init_data_per_variant.as_ref().unwrap()[variant][offset as usize];
    }

    let media_offset = offset - init_len;

    if let Some(ref per_variant) = config.custom_data_per_variant
        && let Some(data) = per_variant.get(variant)
    {
        return data.get(media_offset as usize).copied().unwrap_or(0);
    }

    if let Some(data) = &config.custom_data {
        return data.get(media_offset as usize).copied().unwrap_or(0);
    }

    let segment_size = config.segment_size;
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

// ── Native implementation ──────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{sync::Arc, time::Duration};

    use aes::Aes128;
    use axum::{Router, extract::Path, routing::get};
    use cbc::{
        Encryptor,
        cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7},
    };
    use kithara_test_utils::{TestHttpServer, fixture_protocol::eval_delay};
    use url::Url;

    use super::{super::HlsResult, EncryptionConfig, HlsTestServerConfig, expected_byte_at_impl};

    /// Reusable HLS test server with parametric segment count and size.
    ///
    /// Uses dynamic Axum routes (`/seg/{filename}`) — scales to thousands of segments.
    pub(crate) struct HlsTestServer {
        config: Arc<HlsTestServerConfig>,
        http: TestHttpServer,
    }

    impl HlsTestServer {
        /// Spawn HTTP server on a random local port.
        pub(crate) async fn new(config: HlsTestServerConfig) -> Self {
            let config = Arc::new(config);

            let cfg_master = Arc::clone(&config);
            let cfg_media = Arc::clone(&config);
            let cfg_seg = Arc::clone(&config);
            let cfg_seg_head = Arc::clone(&config);
            let cfg_init = Arc::clone(&config);
            let cfg_key = Arc::clone(&config);

            let seg_route = get(move |Path(filename): Path<String>| {
                let c = Arc::clone(&cfg_seg);
                async move { serve_segment(&c, &filename).await }
            });

            // When head_reported_segment_size is set, HEAD returns a different
            // Content-Length than the actual GET body (simulates HTTP compression).
            let seg_route = if config.head_reported_segment_size.is_some() {
                seg_route.head(move |Path(_filename): Path<String>| {
                    let c = Arc::clone(&cfg_seg_head);
                    async move { serve_segment_head(&c).await }
                })
            } else {
                seg_route
            };

            let app = Router::new()
                .route(
                    "/master.m3u8",
                    get(move || {
                        let c = Arc::clone(&cfg_master);
                        async move { generate_master_playlist(&c) }
                    }),
                )
                .route(
                    "/playlist/{filename}",
                    get(move |Path(filename): Path<String>| {
                        let c = Arc::clone(&cfg_media);
                        let variant = parse_variant_from_playlist(&filename).unwrap_or(0);
                        async move { generate_media_playlist(&c, variant) }
                    }),
                )
                .route("/seg/{filename}", seg_route)
                .route(
                    "/init/{filename}",
                    get(move |Path(filename): Path<String>| {
                        let c = Arc::clone(&cfg_init);
                        async move { serve_init_segment(&c, &filename) }
                    }),
                )
                .route(
                    "/key.bin",
                    get(move || {
                        let c = Arc::clone(&cfg_key);
                        async move { serve_key(&c) }
                    }),
                );

            let http = TestHttpServer::new(app).await;

            Self { config, http }
        }

        /// Build a full URL from a path.
        #[expect(
            clippy::result_large_err,
            reason = "test-only code, ergonomics over size"
        )]
        pub(crate) fn url(&self, path: &str) -> HlsResult<Url> {
            Ok(self.http.url(path))
        }

        /// Access the configuration.
        #[expect(
            dead_code,
            reason = "test utility reserved for future integration tests"
        )]
        pub(crate) fn config(&self) -> &HlsTestServerConfig {
            &self.config
        }

        /// Init segment length for variant 0 (0 if no init data configured).
        pub(crate) fn init_len(&self) -> u64 {
            self.config
                .init_data_per_variant
                .as_ref()
                .and_then(|d| d.first())
                .map_or(0, |d| d.len() as u64)
        }

        /// Total bytes across all segments for one variant (including init segment).
        pub(crate) fn total_bytes(&self) -> u64 {
            self.init_len()
                + self.config.segments_per_variant as u64 * self.config.segment_size as u64
        }

        /// Total duration in seconds for one variant.
        #[expect(
            dead_code,
            reason = "test utility reserved for future integration tests"
        )]
        pub(crate) fn total_duration_secs(&self) -> f64 {
            self.config.segments_per_variant as f64 * self.config.segment_duration_secs
        }

        /// Compute expected byte at a global offset within one variant.
        pub(crate) fn expected_byte_at(&self, variant: usize, offset: u64) -> u8 {
            expected_byte_at_impl(&self.config, variant, offset)
        }
    }

    // Content Generators

    fn generate_master_playlist(config: &HlsTestServerConfig) -> String {
        let mut pl = String::from("#EXTM3U\n#EXT-X-VERSION:6\n");
        for v in 0..config.variant_count {
            let bw = if let Some(ref bandwidths) = config.variant_bandwidths {
                bandwidths
                    .get(v)
                    .copied()
                    .unwrap_or((v + 1) as u64 * 1_280_000)
            } else {
                (v + 1) as u64 * 1_280_000
            };
            pl.push_str(&format!(
                "#EXT-X-STREAM-INF:BANDWIDTH={bw}\nplaylist/v{v}.m3u8\n"
            ));
        }
        pl
    }

    fn generate_media_playlist(config: &HlsTestServerConfig, variant: usize) -> String {
        let dur = config.segment_duration_secs;
        let mut pl = format!(
            "#EXTM3U\n\
             #EXT-X-VERSION:6\n\
             #EXT-X-TARGETDURATION:{}\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-PLAYLIST-TYPE:VOD\n",
            dur.ceil() as u64,
        );
        if config.init_data_per_variant.is_some() {
            pl.push_str(&format!("#EXT-X-MAP:URI=\"../init/v{variant}_init.bin\"\n"));
        }
        if let Some(ref enc) = config.encryption {
            pl.push_str("#EXT-X-KEY:METHOD=AES-128,URI=\"../key.bin\"");
            if let Some(ref iv) = enc.iv {
                pl.push_str(&format!(",IV=0x{}", hex::encode(iv)));
            }
            pl.push('\n');
        }
        for seg in 0..config.segments_per_variant {
            pl.push_str(&format!("#EXTINF:{dur:.1},\n../seg/v{variant}_{seg}.bin\n"));
        }
        pl.push_str("#EXT-X-ENDLIST\n");
        pl
    }

    /// Generate segment data: `V{v}-SEG-{s}:TEST_SEGMENT_DATA` + `0xFF` padding.
    fn generate_segment(variant: usize, segment: usize, size: usize) -> Vec<u8> {
        let prefix = format!("V{variant}-SEG-{segment}:");
        let mut data = prefix.into_bytes();
        data.extend(b"TEST_SEGMENT_DATA");
        if data.len() < size {
            data.resize(size, 0xFF);
        }
        data
    }

    fn parse_variant_from_playlist(filename: &str) -> Option<usize> {
        let name = filename.strip_suffix(".m3u8")?;
        let name = name.strip_prefix('v')?;
        name.parse().ok()
    }

    fn parse_segment_filename(filename: &str) -> Option<(usize, usize)> {
        let name = filename.strip_suffix(".bin").unwrap_or(filename);
        let name = name.strip_prefix('v')?;
        let mut parts = name.split('_');
        let variant: usize = parts.next()?.parse().ok()?;
        let segment: usize = parts.next()?.parse().ok()?;
        Some((variant, segment))
    }

    fn parse_init_filename(filename: &str) -> Option<usize> {
        let name = filename.strip_suffix("_init.bin")?;
        let name = name.strip_prefix('v')?;
        name.parse().ok()
    }

    /// HEAD response for segments with mismatched Content-Length.
    async fn serve_segment_head(
        config: &HlsTestServerConfig,
    ) -> (axum::http::StatusCode, axum::http::HeaderMap) {
        let size = config
            .head_reported_segment_size
            .unwrap_or(config.segment_size);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_LENGTH,
            size.to_string().parse().unwrap(),
        );
        (axum::http::StatusCode::OK, headers)
    }

    async fn serve_segment(config: &HlsTestServerConfig, filename: &str) -> Vec<u8> {
        let (variant, segment) = parse_segment_filename(filename).unwrap_or((0, 0));

        let delay_ms = eval_delay(&config.delay_rules, variant, segment);
        if delay_ms > 0 {
            kithara_platform::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        let plaintext = if let Some(ref per_variant) = config.custom_data_per_variant {
            if let Some(data) = per_variant.get(variant) {
                let start = segment * config.segment_size;
                let end = (start + config.segment_size).min(data.len());
                data[start..end].to_vec()
            } else {
                generate_segment(variant, segment, config.segment_size)
            }
        } else if let Some(data) = &config.custom_data {
            let start = segment * config.segment_size;
            let end = (start + config.segment_size).min(data.len());
            data[start..end].to_vec()
        } else {
            generate_segment(variant, segment, config.segment_size)
        };

        if let Some(ref enc) = config.encryption {
            let iv = derive_iv(enc, segment);
            encrypt_aes128_cbc(&plaintext, &enc.key, &iv)
        } else {
            plaintext
        }
    }

    fn serve_init_segment(config: &HlsTestServerConfig, filename: &str) -> Vec<u8> {
        let variant = parse_init_filename(filename).unwrap_or(0);

        let plaintext = if let Some(ref init_data) = config.init_data_per_variant {
            if let Some(data) = init_data.get(variant) {
                data.as_ref().clone()
            } else {
                return Vec::new();
            }
        } else {
            return Vec::new();
        };

        if let Some(ref enc) = config.encryption {
            let iv = derive_iv(enc, 0);
            encrypt_aes128_cbc(&plaintext, &enc.key, &iv)
        } else {
            plaintext
        }
    }

    fn serve_key(config: &HlsTestServerConfig) -> Vec<u8> {
        config
            .encryption
            .as_ref()
            .map(|enc| enc.key.to_vec())
            .unwrap_or_default()
    }

    fn derive_iv(enc: &EncryptionConfig, sequence: usize) -> [u8; 16] {
        if let Some(iv) = enc.iv {
            iv
        } else {
            let mut iv = [0u8; 16];
            iv[8..16].copy_from_slice(&(sequence as u64).to_be_bytes());
            iv
        }
    }

    fn encrypt_aes128_cbc(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
        let encryptor = Encryptor::<Aes128>::new(key.into(), iv.into());
        let padded_len = data.len() + (16 - data.len() % 16);
        let mut buf = vec![0u8; padded_len];
        buf[..data.len()].copy_from_slice(data);
        let ct = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
            .expect("encrypt_padded_mut");
        ct.to_vec()
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::HlsTestServer;

// ── WASM implementation ────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
mod wasm {
    use kithara_test_utils::{
        fixture_client,
        fixture_protocol::{DataMode, HlsSessionConfig, InitMode},
    };
    use url::Url;

    use super::{super::HlsResult, HlsTestServerConfig, expected_byte_at_impl};

    /// Remote HLS test server backed by fixture server session.
    pub(crate) struct HlsTestServer {
        session_id: String,
        base_url: Url,
        config: HlsTestServerConfig,
        remote_init_len: u64,
        remote_total_bytes: u64,
    }

    impl HlsTestServer {
        pub(crate) async fn new(config: HlsTestServerConfig) -> Self {
            // Convert to serializable config for fixture server.
            let data_mode = if let Some(per_variant) = &config.custom_data_per_variant {
                DataMode::CustomDataPerVariant(
                    per_variant
                        .iter()
                        .map(|variant_data| variant_data.as_ref().clone())
                        .collect(),
                )
            } else if let Some(data) = &config.custom_data {
                DataMode::CustomData(data.as_ref().clone())
            } else {
                DataMode::TestPattern
            };

            let session_config = HlsSessionConfig {
                variant_count: config.variant_count,
                segments_per_variant: config.segments_per_variant,
                segment_size: config.segment_size,
                segment_duration_secs: config.segment_duration_secs,
                data_mode,
                init_mode: config
                    .init_data_per_variant
                    .as_ref()
                    .map(|init_data| {
                        InitMode::Custom(
                            init_data
                                .iter()
                                .map(|segment| segment.as_ref().clone())
                                .collect(),
                        )
                    })
                    .unwrap_or(InitMode::None),
                variant_bandwidths: config.variant_bandwidths.clone(),
                delay_rules: config.delay_rules.clone(),
                encryption: config.encryption.as_ref().map(|enc| {
                    kithara_test_utils::fixture_protocol::EncryptionRequest {
                        key_hex: hex::encode(enc.key),
                        iv_hex: enc.iv.map(hex::encode),
                    }
                }),
                head_reported_segment_size: config.head_reported_segment_size,
            };

            let resp = fixture_client::create_hls_session(&session_config).await;
            Self {
                session_id: resp.session_id,
                base_url: resp.base_url.parse().unwrap(),
                config,
                remote_init_len: resp.init_len,
                remote_total_bytes: resp.total_bytes,
            }
        }

        #[expect(
            clippy::result_large_err,
            reason = "test-only code, ergonomics over size"
        )]
        pub(crate) fn url(&self, path: &str) -> HlsResult<Url> {
            Ok(self.base_url.join(path).unwrap())
        }

        pub(crate) fn init_len(&self) -> u64 {
            self.remote_init_len
        }

        pub(crate) fn total_bytes(&self) -> u64 {
            self.remote_total_bytes
        }

        pub(crate) fn expected_byte_at(&self, variant: usize, offset: u64) -> u8 {
            expected_byte_at_impl(&self.config, variant, offset)
        }
    }

    impl Drop for HlsTestServer {
        fn drop(&mut self) {
            let id = self.session_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                fixture_client::delete_session(&id).await;
            });
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::HlsTestServer;
