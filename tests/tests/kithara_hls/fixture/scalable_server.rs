//! Configurable HLS test server with dynamic routing.
//!
//! Serves any number of variants and segments with deterministic data.
//! Segment data format: `V{variant}-SEG-{segment}:TEST_SEGMENT_DATA` + `0xFF` padding.
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

use std::{sync::Arc, time::Duration};

use axum::{Router, extract::Path, routing::get};
use kithara_hls::HlsError;
use tokio::net::TcpListener;
use url::Url;

use super::HlsResult;

// ==================== Config ====================

/// Configuration for [`HlsTestServer`].
pub struct HlsTestServerConfig {
    /// Number of HLS variants (bitrate levels). Default: 1.
    pub variant_count: usize,
    /// Number of segments per variant. Default: 3.
    pub segments_per_variant: usize,
    /// Segment size in bytes. Default: 200 000 (200 KB).
    pub segment_size: usize,
    /// Segment duration in seconds (for playlist `#EXTINF`). Default: 4.0.
    pub segment_duration_secs: f64,
    /// Custom binary data to serve instead of generated test patterns.
    ///
    /// When set, segments are sliced from this data: segment N serves
    /// `data[N * segment_size .. (N+1) * segment_size]`.
    /// Total length must equal `segments_per_variant * segment_size`.
    pub custom_data: Option<Arc<Vec<u8>>>,
    /// Per-variant binary data for media segments.
    ///
    /// Segment N of variant V serves:
    /// `data[V][N * segment_size .. (N+1) * segment_size]`.
    /// Takes priority over `custom_data` when set.
    pub custom_data_per_variant: Option<Vec<Arc<Vec<u8>>>>,
    /// Per-variant init segment data (served via `#EXT-X-MAP` route).
    ///
    /// When set, playlists include `#EXT-X-MAP:URI="../init/v{variant}_init.bin"`.
    pub init_data_per_variant: Option<Vec<Arc<Vec<u8>>>>,
    /// Custom bandwidths for master playlist variants.
    ///
    /// When `None`, uses default `(v + 1) * 1_280_000`.
    pub variant_bandwidths: Option<Vec<u64>>,
    /// Async delay function: `(variant, segment_index)` → `Duration` to sleep before serving.
    ///
    /// Used to simulate slow segments and trigger ABR switches.
    pub segment_delay: Option<Arc<dyn Fn(usize, usize) -> Duration + Send + Sync>>,
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
            segment_delay: None,
        }
    }
}

// ==================== Server ====================

/// Reusable HLS test server with parametric segment count and size.
///
/// Uses dynamic Axum routes (`/seg/{filename}`) — scales to thousands of segments.
pub struct HlsTestServer {
    base_url: String,
    config: Arc<HlsTestServerConfig>,
}

impl HlsTestServer {
    /// Spawn HTTP server on a random local port.
    pub async fn new(config: HlsTestServerConfig) -> Self {
        let config = Arc::new(config);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind HlsTestServer");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://127.0.0.1:{}", addr.port());

        let cfg_master = Arc::clone(&config);
        let cfg_media = Arc::clone(&config);
        let cfg_seg = Arc::clone(&config);
        let cfg_init = Arc::clone(&config);

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
            .route(
                "/seg/{filename}",
                get(move |Path(filename): Path<String>| {
                    let c = Arc::clone(&cfg_seg);
                    async move { serve_segment(&c, &filename).await }
                }),
            )
            .route(
                "/init/{filename}",
                get(move |Path(filename): Path<String>| {
                    let c = Arc::clone(&cfg_init);
                    async move { serve_init_segment(&c, &filename) }
                }),
            );

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve HlsTestServer");
        });

        Self { base_url, config }
    }

    /// Build a full URL from a path.
    pub fn url(&self, path: &str) -> HlsResult<Url> {
        format!("{}{}", self.base_url, path)
            .parse()
            .map_err(|e| HlsError::InvalidUrl(format!("Invalid test URL: {e}")))
    }

    /// Access the configuration.
    #[allow(dead_code)]
    pub fn config(&self) -> &HlsTestServerConfig {
        &self.config
    }

    /// Total bytes across all segments for one variant.
    pub fn total_bytes(&self) -> u64 {
        self.config.segments_per_variant as u64 * self.config.segment_size as u64
    }

    /// Total duration in seconds for one variant.
    #[allow(dead_code)]
    pub fn total_duration_secs(&self) -> f64 {
        self.config.segments_per_variant as f64 * self.config.segment_duration_secs
    }

    /// Compute expected byte at a global offset within one variant.
    ///
    /// When `custom_data` is set, returns the byte from that data.
    /// Otherwise returns bytes from the generated test pattern (prefix + `0xFF` padding).
    pub fn expected_byte_at(&self, variant: usize, offset: u64) -> u8 {
        expected_byte_at_impl(&self.config, variant, offset)
    }
}

// ==================== Content Generators ====================

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

/// Parse `v{variant}.m3u8` → variant index.
fn parse_variant_from_playlist(filename: &str) -> Option<usize> {
    let name = filename.strip_suffix(".m3u8")?;
    let name = name.strip_prefix('v')?;
    name.parse().ok()
}

/// Parse `v{variant}_{segment}.bin` → `(variant, segment)`.
fn parse_segment_filename(filename: &str) -> Option<(usize, usize)> {
    let name = filename.strip_suffix(".bin").unwrap_or(filename);
    let name = name.strip_prefix('v')?;
    let mut parts = name.split('_');
    let variant: usize = parts.next()?.parse().ok()?;
    let segment: usize = parts.next()?.parse().ok()?;
    Some((variant, segment))
}

/// Parse `v{variant}_init.bin` → variant index.
fn parse_init_filename(filename: &str) -> Option<usize> {
    let name = filename.strip_suffix("_init.bin")?;
    let name = name.strip_prefix('v')?;
    name.parse().ok()
}

async fn serve_segment(config: &HlsTestServerConfig, filename: &str) -> Vec<u8> {
    let (variant, segment) = parse_segment_filename(filename).unwrap_or((0, 0));

    if let Some(ref delay_fn) = config.segment_delay {
        let delay = delay_fn(variant, segment);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }

    if let Some(ref per_variant) = config.custom_data_per_variant {
        if let Some(data) = per_variant.get(variant) {
            let start = segment * config.segment_size;
            let end = (start + config.segment_size).min(data.len());
            return data[start..end].to_vec();
        }
    }

    if let Some(data) = &config.custom_data {
        let start = segment * config.segment_size;
        let end = (start + config.segment_size).min(data.len());
        return data[start..end].to_vec();
    }

    generate_segment(variant, segment, config.segment_size)
}

fn serve_init_segment(config: &HlsTestServerConfig, filename: &str) -> Vec<u8> {
    let variant = parse_init_filename(filename).unwrap_or(0);

    if let Some(ref init_data) = config.init_data_per_variant {
        if let Some(data) = init_data.get(variant) {
            return data.as_ref().clone();
        }
    }

    Vec::new()
}

/// Compute expected byte for verification (shared logic for server + tests).
fn expected_byte_at_impl(config: &HlsTestServerConfig, variant: usize, offset: u64) -> u8 {
    if let Some(ref per_variant) = config.custom_data_per_variant {
        if let Some(data) = per_variant.get(variant) {
            return data.get(offset as usize).copied().unwrap_or(0);
        }
    }

    if let Some(data) = &config.custom_data {
        return data.get(offset as usize).copied().unwrap_or(0);
    }

    let segment_size = config.segment_size;
    let seg_idx = (offset / segment_size as u64) as usize;
    let off_in_seg = (offset % segment_size as u64) as usize;

    let prefix = format!("V{variant}-SEG-{seg_idx}:TEST_SEGMENT_DATA");
    let prefix_bytes = prefix.as_bytes();

    if off_in_seg < prefix_bytes.len() {
        prefix_bytes[off_in_seg]
    } else {
        0xFF
    }
}
