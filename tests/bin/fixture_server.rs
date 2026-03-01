//! Dynamic fixture server for WASM and native integration tests.
//!
//! Supports session-based dynamic configuration via HTTP API
//! and backwards-compatible static routes for existing WASM stress tests.
//!
//! # API
//!
//! - `GET  /health`                — readiness check
//! - `POST /session/hls-fixed`     — create fixed HLS session (`TestServer`)
//! - `POST /session/hls`           — create configurable HLS session (`HlsTestServer`)
//! - `POST /session/abr`           — create ABR session (`AbrTestServer`)
//! - `POST /session/audio-fixtures` — create audio fixtures session
//! - `POST /session/file`          — create file download session
//! - `DELETE /session/{id}`        — delete session
//! - `GET  /s/{id}/master.m3u8`    — master playlist
//! - `GET  /s/{id}/playlist/v{v}.m3u8` — media playlist
//! - `GET  /s/{id}/v{v}.m3u8`      — media playlist (alt path for `TestServer`)
//! - `GET  /s/{id}/seg/v{v}_{s}.bin`   — segment data
//! - `GET  /s/{id}/init/v{v}_init.bin` — init segment
//! - `GET  /s/{id}/init/v{v}.bin`      — init segment (alt path for ABR)
//! - `GET  /s/{id}/key.bin`        — encryption key
//! - `GET  /s/{id}/aes/key.bin`    — AES-128 key
//! - `GET  /s/{id}/aes/seg0.bin`   — AES-128 encrypted segment
//! - `GET  /s/{id}/audio/{filename}` — audio fixture file (silence.wav, test.mp3)
//! - `GET  /s/{id}/file/{filename}` — file download with range support
//! - `HEAD /s/{id}/file/{filename}` — file HEAD (Content-Length, Accept-Ranges)
//!
//! Static routes (backwards compatibility):
//! - `GET  /master.m3u8`, `/master-jitter.m3u8`, `/playlist/{f}`, `/seg/{f}`

// On wasm32, provide a no-op main.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("FIXTURE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3333);
    server::run(port).await;
}

#[cfg(not(target_arch = "wasm32"))]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::unwrap_used,
    reason = "test binary — casts and unwraps are acceptable"
)]
mod server {
    use std::{
        collections::HashMap,
        sync::Arc,
        time::{Duration, Instant},
    };

    use axum::{
        Router,
        body::Body,
        extract::{Path, State},
        http::{HeaderMap, Response, StatusCode, header},
        routing::{delete, get, post},
    };
    use kithara_test_utils::fixture_protocol::{
        AbrSessionConfig, DataMode, EncryptionRequest, FileSessionConfig, HlsSessionConfig,
        HttpTestSessionConfig, InitMode, PcmPattern, SessionResponse, create_pcm_segments,
        create_wav_init_header, eval_delay, generate_segment,
    };
    use tokio::{net::TcpListener, sync::RwLock};
    use tower_http::{cors::CorsLayer, services::ServeDir};

    // ── Session Types ──────────────────────────────────────────────

    enum SessionKind {
        FixedHls(FixedHlsData),
        Hls(Box<HlsData>),
        Abr(AbrData),
        AudioFixtures(AudioFixturesData),
        File(Box<FileData>),
        HttpTest(Box<HttpTestData>),
    }

    struct Session {
        kind: SessionKind,
        created: Instant,
    }

    /// Pre-generated data for a fixed HLS session.
    struct FixedHlsData;

    /// Pre-generated data for a configurable HLS session.
    struct HlsData {
        config: HlsSessionConfig,
        /// Per-variant media segment data. `segments[variant]` is the concatenated bytes.
        segments: Vec<Vec<u8>>,
        /// Per-variant init segment data (empty if no init).
        inits: Vec<Vec<u8>>,
        /// Encryption key bytes (empty if no encryption).
        key: Vec<u8>,
    }

    /// Pre-generated data for an ABR session.
    struct AbrData {
        config: AbrSessionConfig,
    }

    /// Pre-loaded audio fixture data.
    struct AudioFixturesData {
        wav: &'static [u8],
        mp3: &'static [u8],
    }

    /// Pre-loaded file data for download tests.
    struct FileData {
        config: FileSessionConfig,
        /// Tracks how many sequential (non-range) GET requests have been made per file.
        sequential_count: std::sync::atomic::AtomicUsize,
    }

    /// Data for generic HTTP test sessions.
    struct HttpTestData {
        config: HttpTestSessionConfig,
        /// Per-route request counters (indexed by route path).
        route_counts: HashMap<String, std::sync::atomic::AtomicUsize>,
    }

    type Sessions = Arc<RwLock<HashMap<String, Session>>>;

    // ── App State ──────────────────────────────────────────────────

    #[derive(Clone)]
    struct AppState {
        sessions: Sessions,
        /// Backwards-compatible static data for WASM stress tests.
        static_uniform: Arc<StaticFixture>,
        static_jitter: Arc<StaticFixture>,
        port: u16,
    }

    struct StaticFixture {
        wav_data: Vec<u8>,
        segment_ranges: Vec<(usize, usize)>,
        segment_durations_secs: Vec<f64>,
    }

    // ── Static fixture generation (backwards compat) ───────────────

    const SAMPLE_RATE: u32 = 44100;
    const CHANNELS: u16 = 2;
    const STATIC_SEGMENT_COUNT: usize = 100;
    const UNIFORM_SEGMENT_SIZE: usize = 200_000;
    const JITTER_MIN_SEGMENT_SIZE: usize = 140_000;
    const JITTER_SIZE_SPAN: usize = 120_000;
    const SESSION_TTL_SECS: u64 = 120;

    const SILENCE_WAV: &[u8] = include_bytes!("../../assets/silence_1s.wav");
    const TEST_MP3: &[u8] = include_bytes!("../../assets/test.mp3");

    fn assets_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root")
            .join("assets")
    }

    fn create_saw_wav(total_bytes: usize) -> Vec<u8> {
        kithara_test_utils::create_saw_wav(total_bytes)
    }

    fn build_static_fixture(segment_sizes: &[usize]) -> StaticFixture {
        let total_bytes: usize = segment_sizes.iter().copied().sum();
        let wav_data = create_saw_wav(total_bytes);
        let bytes_per_second = f64::from(SAMPLE_RATE) * f64::from(CHANNELS) * 2.0;

        let mut segment_ranges = Vec::with_capacity(segment_sizes.len());
        let mut segment_durations_secs = Vec::with_capacity(segment_sizes.len());
        let mut start = 0usize;
        for size in segment_sizes {
            let end = (start + *size).min(wav_data.len());
            segment_ranges.push((start, end));
            segment_durations_secs.push((end.saturating_sub(start)) as f64 / bytes_per_second);
            start = end;
        }

        StaticFixture {
            wav_data,
            segment_ranges,
            segment_durations_secs,
        }
    }

    fn jitter_segment_sizes() -> Vec<usize> {
        (0..STATIC_SEGMENT_COUNT)
            .map(|i| JITTER_MIN_SEGMENT_SIZE + ((i * 7919) % JITTER_SIZE_SPAN))
            .collect()
    }

    fn static_media_playlist(fixture: &StaticFixture, prefix: &str) -> String {
        let target_dur = fixture
            .segment_durations_secs
            .iter()
            .copied()
            .fold(0.0_f64, f64::max)
            .ceil() as u64;
        let mut pl = format!(
            "#EXTM3U\n\
             #EXT-X-VERSION:6\n\
             #EXT-X-TARGETDURATION:{target_dur}\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-PLAYLIST-TYPE:VOD\n",
        );
        for (seg, dur) in fixture.segment_durations_secs.iter().copied().enumerate() {
            pl.push_str(&format!("#EXTINF:{dur:.3},\n../seg/{prefix}{seg}.bin\n"));
        }
        pl.push_str("#EXT-X-ENDLIST\n");
        pl
    }

    // ── HLS data generation ────────────────────────────────────────

    fn generate_hls_data(config: &HlsSessionConfig) -> HlsData {
        let mut segments = Vec::with_capacity(config.variant_count);
        let mut inits = Vec::with_capacity(config.variant_count);

        for v in 0..config.variant_count {
            let variant_data = match &config.data_mode {
                DataMode::TestPattern => {
                    let mut data = Vec::new();
                    for s in 0..config.segments_per_variant {
                        data.extend(generate_segment(v, s, config.segment_size));
                    }
                    data
                }
                DataMode::CustomData(data) => data.clone(),
                DataMode::CustomDataPerVariant(patterns) => {
                    patterns.get(v).cloned().unwrap_or_default()
                }
                DataMode::SawWav { .. } => {
                    let total = config.segments_per_variant * config.segment_size;
                    create_saw_wav(total)
                }
                DataMode::PerVariantPcm {
                    channels, patterns, ..
                } => {
                    let pattern = patterns.get(v).unwrap_or(&PcmPattern::Ascending);
                    create_pcm_segments(
                        pattern,
                        *channels,
                        config.segments_per_variant,
                        config.segment_size,
                    )
                }
            };
            segments.push(variant_data);

            let init = match &config.init_mode {
                InitMode::None => Vec::new(),
                InitMode::WavHeader {
                    sample_rate,
                    channels,
                } => create_wav_init_header(*sample_rate, *channels),
                InitMode::Custom(data) => data.get(v).cloned().unwrap_or_default(),
            };
            inits.push(init);
        }

        let key = config
            .encryption
            .as_ref()
            .map(|enc| hex::decode(&enc.key_hex).unwrap_or_default())
            .unwrap_or_default();

        HlsData {
            config: config.clone(),
            segments,
            inits,
            key,
        }
    }

    // ── Fixed HLS helpers ──────────────────────────────────────────

    fn fixed_test_segment_data(variant: usize, segment: usize) -> Vec<u8> {
        generate_segment(variant, segment, 200_000)
    }

    fn fixed_test_init_data(variant: usize) -> Vec<u8> {
        let prefix = format!("V{variant}-INIT:");
        let mut data = prefix.into_bytes();
        data.extend(b"TEST_INIT_DATA");
        data
    }

    fn fixed_test_key_data() -> Vec<u8> {
        b"TEST_KEY_DATA_123456".to_vec()
    }

    fn fixed_master_playlist() -> &'static str {
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

    fn fixed_master_playlist_with_init() -> &'static str {
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

    fn fixed_master_playlist_encrypted() -> &'static str {
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=854x480,CODECS="avc1.42c01e,mp4a.40.2"
v0-encrypted.m3u8
"#
    }

    fn fixed_media_playlist(variant: usize) -> String {
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

    fn fixed_media_playlist_with_init(variant: usize) -> String {
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

    fn fixed_media_playlist_encrypted() -> String {
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

    fn fixed_aes128_key_bytes() -> Vec<u8> {
        b"0123456789abcdef".to_vec()
    }

    fn fixed_aes128_ciphertext() -> Vec<u8> {
        use aes::Aes128;
        use cbc::{
            Encryptor,
            cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7},
        };
        let key = fixed_aes128_key_bytes();
        let iv = [0u8; 16];
        let mut data = b"V0-SEG-0:DRM-PLAINTEXT".to_vec();
        let plain_len = data.len();
        data.resize(plain_len + 16, 0);
        let encryptor = Encryptor::<Aes128>::new((&key[..16]).into(), (&iv).into());
        let cipher = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut data, plain_len)
            .expect("aes128 encrypt");
        cipher.to_vec()
    }

    // ── HLS playlist generation ────────────────────────────────────

    fn hls_master_playlist(config: &HlsSessionConfig) -> String {
        let mut pl = String::from("#EXTM3U\n#EXT-X-VERSION:6\n");
        for v in 0..config.variant_count {
            let bw = config
                .variant_bandwidths
                .as_ref()
                .and_then(|b| b.get(v).copied())
                .unwrap_or((v + 1) as u64 * 1_280_000);
            pl.push_str(&format!(
                "#EXT-X-STREAM-INF:BANDWIDTH={bw}\nplaylist/v{v}.m3u8\n"
            ));
        }
        pl
    }

    fn hls_media_playlist(config: &HlsSessionConfig, variant: usize) -> String {
        let dur = config.segment_duration_secs;
        let mut pl = format!(
            "#EXTM3U\n\
             #EXT-X-VERSION:6\n\
             #EXT-X-TARGETDURATION:{}\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-PLAYLIST-TYPE:VOD\n",
            dur.ceil() as u64,
        );
        if !matches!(config.init_mode, InitMode::None) {
            pl.push_str(&format!("#EXT-X-MAP:URI=\"../init/v{variant}_init.bin\"\n"));
        }
        if let Some(ref enc) = config.encryption {
            pl.push_str("#EXT-X-KEY:METHOD=AES-128,URI=\"../key.bin\"");
            if let Some(ref iv) = enc.iv_hex {
                pl.push_str(&format!(",IV=0x{iv}"));
            }
            pl.push('\n');
        }
        for seg in 0..config.segments_per_variant {
            pl.push_str(&format!("#EXTINF:{dur:.1},\n../seg/v{variant}_{seg}.bin\n"));
        }
        pl.push_str("#EXT-X-ENDLIST\n");
        pl
    }

    // ── ABR helpers ────────────────────────────────────────────────

    fn abr_media_playlist(variant: usize, has_init: bool) -> String {
        let mut s = String::new();
        s.push_str(
            "#EXTM3U\n\
             #EXT-X-VERSION:6\n\
             #EXT-X-TARGETDURATION:4\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-PLAYLIST-TYPE:VOD\n",
        );
        if has_init {
            s.push_str(&format!("#EXT-X-MAP:URI=\"init/v{variant}.bin\"\n"));
        }
        for i in 0..3 {
            s.push_str("#EXTINF:4.0,\n");
            s.push_str(&format!("seg/v{variant}_{i}.bin\n"));
        }
        s.push_str("#EXT-X-ENDLIST\n");
        s
    }

    fn abr_init_data(variant: usize) -> Vec<u8> {
        format!("V{variant}-INIT:").into_bytes()
    }

    async fn abr_segment_data(
        variant: usize,
        segment: usize,
        delay: Duration,
        total_len: usize,
    ) -> Vec<u8> {
        if delay != Duration::ZERO {
            kithara_platform::time::sleep(delay).await;
        }
        let mut data = Vec::new();
        data.push(variant as u8);
        data.extend(&(segment as u32).to_be_bytes());
        let header_size = 1 + 4 + 4;
        let data_len = total_len.saturating_sub(header_size);
        data.extend(&(data_len as u32).to_be_bytes());
        data.extend(std::iter::repeat_n(b'A', data_len));
        data
    }

    // ── AES-128 encryption ─────────────────────────────────────────

    fn encrypt_aes128_cbc(data: &[u8], key: &[u8], iv: &[u8; 16]) -> Vec<u8> {
        use aes::Aes128;
        use cbc::{
            Encryptor,
            cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7},
        };
        let key: [u8; 16] = key[..16].try_into().unwrap();
        let encryptor = Encryptor::<Aes128>::new((&key).into(), iv.into());
        let padded_len = data.len() + (16 - data.len() % 16);
        let mut buf = vec![0u8; padded_len];
        buf[..data.len()].copy_from_slice(data);
        let ct = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut buf, data.len())
            .expect("encrypt_padded_mut");
        ct.to_vec()
    }

    fn derive_iv(enc: &EncryptionRequest, sequence: usize) -> [u8; 16] {
        enc.iv_hex.as_ref().map_or_else(
            || {
                let mut iv = [0u8; 16];
                iv[8..16].copy_from_slice(&(sequence as u64).to_be_bytes());
                iv
            },
            |iv_hex| {
                let mut iv = [0u8; 16];
                let decoded = hex::decode(iv_hex).unwrap_or_default();
                let len = decoded.len().min(16);
                iv[..len].copy_from_slice(&decoded[..len]);
                iv
            },
        )
    }

    // ── Range request support ──────────────────────────────────────

    fn parse_range_header(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
        let value = headers.get(header::RANGE)?.to_str().ok()?.trim();
        let range = value.strip_prefix("bytes=")?;
        let mut parts = range.splitn(2, '-');
        let start = parts.next()?.trim().parse::<u64>().ok()?;
        let end_str = parts.next()?.trim();
        let end = if end_str.is_empty() {
            None
        } else {
            Some(end_str.parse::<u64>().ok()?.saturating_add(1))
        };
        Some((start, end))
    }

    fn build_range_response(
        data: &[u8],
        headers: &HeaderMap,
        include_body: bool,
    ) -> Response<Body> {
        let total = data.len();
        let range = parse_range_header(headers);
        if let Some((start, end_opt)) = range {
            if start >= total as u64 {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                    .body(Body::empty())
                    .unwrap();
            }
            let end = end_opt.unwrap_or(total as u64).min(total as u64);
            if start >= end {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                    .body(Body::empty())
                    .unwrap();
            }
            let s = start as usize;
            let e = end as usize;
            let status = if s == 0 && e == total {
                StatusCode::OK
            } else {
                StatusCode::PARTIAL_CONTENT
            };
            let mut builder = Response::builder()
                .status(status)
                .header(header::CONTENT_LENGTH, (e - s).to_string())
                .header(header::ACCEPT_RANGES, "bytes");
            if status == StatusCode::PARTIAL_CONTENT {
                builder = builder.header(
                    header::CONTENT_RANGE,
                    format!("bytes {}-{}/{}", s, e.saturating_sub(1), total),
                );
            }
            let body = if include_body {
                Body::from(data[s..e].to_vec())
            } else {
                Body::empty()
            };
            builder.body(body).unwrap()
        } else {
            let body = if include_body {
                Body::from(data.to_vec())
            } else {
                Body::empty()
            };
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, total.to_string())
                .header(header::ACCEPT_RANGES, "bytes")
                .body(body)
                .unwrap()
        }
    }

    // ── Route Handlers ─────────────────────────────────────────────

    async fn health() -> &'static str {
        "ok"
    }

    async fn create_fixed_hls_session(
        State(state): State<AppState>,
    ) -> axum::Json<SessionResponse> {
        let id = uuid::Uuid::new_v4().to_string();
        let base_url = format!("http://127.0.0.1:{}/s/{id}", state.port);
        let session = Session {
            kind: SessionKind::FixedHls(FixedHlsData),
            created: Instant::now(),
        };
        state.sessions.write().await.insert(id.clone(), session);
        axum::Json(SessionResponse {
            session_id: id,
            base_url,
            total_bytes: 3 * 200_000, // 3 segments × 200KB
            init_len: 0,
        })
    }

    async fn create_hls_session(
        State(state): State<AppState>,
        body: String,
    ) -> Result<axum::Json<SessionResponse>, StatusCode> {
        let config: HlsSessionConfig =
            serde_json::from_str(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
        let data = generate_hls_data(&config);
        let init_len = data.inits.first().map_or(0, |i| i.len() as u64);
        let total_bytes =
            init_len + config.segments_per_variant as u64 * config.segment_size as u64;
        let id = uuid::Uuid::new_v4().to_string();
        let base_url = format!("http://127.0.0.1:{}/s/{id}", state.port);
        let session = Session {
            kind: SessionKind::Hls(Box::new(data)),
            created: Instant::now(),
        };
        state.sessions.write().await.insert(id.clone(), session);
        Ok(axum::Json(SessionResponse {
            session_id: id,
            base_url,
            total_bytes,
            init_len,
        }))
    }

    async fn create_abr_session(
        State(state): State<AppState>,
        body: String,
    ) -> Result<axum::Json<SessionResponse>, StatusCode> {
        let config: AbrSessionConfig =
            serde_json::from_str(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
        let id = uuid::Uuid::new_v4().to_string();
        let base_url = format!("http://127.0.0.1:{}/s/{id}", state.port);
        let session = Session {
            kind: SessionKind::Abr(AbrData { config }),
            created: Instant::now(),
        };
        state.sessions.write().await.insert(id.clone(), session);
        Ok(axum::Json(SessionResponse {
            session_id: id,
            base_url,
            total_bytes: 3 * 200_000,
            init_len: 0,
        }))
    }

    async fn create_audio_fixtures_session(
        State(state): State<AppState>,
    ) -> axum::Json<SessionResponse> {
        let id = uuid::Uuid::new_v4().to_string();
        let base_url = format!("http://127.0.0.1:{}/s/{id}", state.port);
        let session = Session {
            kind: SessionKind::AudioFixtures(AudioFixturesData {
                wav: SILENCE_WAV,
                mp3: TEST_MP3,
            }),
            created: Instant::now(),
        };
        state.sessions.write().await.insert(id.clone(), session);
        axum::Json(SessionResponse {
            session_id: id,
            base_url,
            total_bytes: 0,
            init_len: 0,
        })
    }

    async fn create_file_session(
        State(state): State<AppState>,
        body: String,
    ) -> Result<axum::Json<SessionResponse>, StatusCode> {
        let config: FileSessionConfig =
            serde_json::from_str(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
        let id = uuid::Uuid::new_v4().to_string();
        let base_url = format!("http://127.0.0.1:{}/s/{id}", state.port);
        let session = Session {
            kind: SessionKind::File(Box::new(FileData {
                config,
                sequential_count: std::sync::atomic::AtomicUsize::new(0),
            })),
            created: Instant::now(),
        };
        state.sessions.write().await.insert(id.clone(), session);
        Ok(axum::Json(SessionResponse {
            session_id: id,
            base_url,
            total_bytes: 0,
            init_len: 0,
        }))
    }

    async fn create_http_test_session(
        State(state): State<AppState>,
        body: String,
    ) -> Result<axum::Json<SessionResponse>, StatusCode> {
        let config: HttpTestSessionConfig =
            serde_json::from_str(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
        let id = uuid::Uuid::new_v4().to_string();
        let base_url = format!("http://127.0.0.1:{}/s/{id}", state.port);
        let route_counts = config
            .routes
            .iter()
            .map(|r| (r.path.clone(), std::sync::atomic::AtomicUsize::new(0)))
            .collect();
        let session = Session {
            kind: SessionKind::HttpTest(Box::new(HttpTestData {
                config,
                route_counts,
            })),
            created: Instant::now(),
        };
        state.sessions.write().await.insert(id.clone(), session);
        Ok(axum::Json(SessionResponse {
            session_id: id,
            base_url,
            total_bytes: 0,
            init_len: 0,
        }))
    }

    async fn delete_session_handler(
        State(state): State<AppState>,
        Path(id): Path<String>,
    ) -> StatusCode {
        if state.sessions.write().await.remove(&id).is_some() {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::NOT_FOUND
        }
    }

    // ── Session-based route handlers ───────────────────────────────

    #[expect(clippy::significant_drop_tightening)]
    async fn session_master(
        State(state): State<AppState>,
        Path(id): Path<String>,
    ) -> Result<String, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        match &session.kind {
            SessionKind::FixedHls(_) => Ok(fixed_master_playlist().to_string()),
            SessionKind::Hls(data) => Ok(hls_master_playlist(&data.config)),
            SessionKind::Abr(data) => Ok(data.config.master_playlist.clone()),
            SessionKind::AudioFixtures(_) | SessionKind::File(_) | SessionKind::HttpTest(_) => {
                Err(StatusCode::NOT_FOUND)
            }
        }
    }

    #[expect(clippy::significant_drop_tightening)]
    async fn session_master_init(
        State(state): State<AppState>,
        Path(id): Path<String>,
    ) -> Result<String, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        match &session.kind {
            SessionKind::FixedHls(_) => Ok(fixed_master_playlist_with_init().to_string()),
            SessionKind::Hls(_)
            | SessionKind::Abr(_)
            | SessionKind::AudioFixtures(_)
            | SessionKind::File(_)
            | SessionKind::HttpTest(_) => Err(StatusCode::NOT_FOUND),
        }
    }

    #[expect(clippy::significant_drop_tightening)]
    async fn session_master_encrypted(
        State(state): State<AppState>,
        Path(id): Path<String>,
    ) -> Result<String, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        match &session.kind {
            SessionKind::FixedHls(_) => Ok(fixed_master_playlist_encrypted().to_string()),
            SessionKind::Hls(_)
            | SessionKind::Abr(_)
            | SessionKind::AudioFixtures(_)
            | SessionKind::File(_)
            | SessionKind::HttpTest(_) => Err(StatusCode::NOT_FOUND),
        }
    }

    #[expect(clippy::significant_drop_tightening)]
    async fn session_playlist(
        State(state): State<AppState>,
        Path((id, filename)): Path<(String, String)>,
    ) -> Result<String, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        let variant = parse_variant_from_playlist(&filename).unwrap_or(0);
        match &session.kind {
            SessionKind::FixedHls(_)
            | SessionKind::AudioFixtures(_)
            | SessionKind::File(_)
            | SessionKind::HttpTest(_) => Err(StatusCode::NOT_FOUND),
            SessionKind::Hls(data) => Ok(hls_media_playlist(&data.config, variant)),
            SessionKind::Abr(data) => Ok(abr_media_playlist(variant, data.config.has_init)),
        }
    }

    /// Fixed HLS: /s/{id}/v{v}.m3u8, /s/{id}/v{v}-init.m3u8, /s/{id}/v{v}-encrypted.m3u8
    #[expect(clippy::significant_drop_tightening)]
    async fn session_fixed_variant_playlist(
        State(state): State<AppState>,
        Path((id, filename)): Path<(String, String)>,
    ) -> Result<String, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        match &session.kind {
            SessionKind::FixedHls(_) => {
                if filename == "v0-encrypted.m3u8" {
                    return Ok(fixed_media_playlist_encrypted());
                }
                if let Some(rest) = filename.strip_suffix("-init.m3u8") {
                    let variant: usize = rest
                        .strip_prefix('v')
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    return Ok(fixed_media_playlist_with_init(variant));
                }
                if let Some(rest) = filename.strip_suffix(".m3u8") {
                    let variant: usize = rest
                        .strip_prefix('v')
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    return Ok(fixed_media_playlist(variant));
                }
                Err(StatusCode::NOT_FOUND)
            }
            SessionKind::Abr(data) => {
                let variant = filename
                    .strip_suffix(".m3u8")
                    .and_then(|s| s.strip_prefix('v'))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                Ok(abr_media_playlist(variant, data.config.has_init))
            }
            SessionKind::Hls(_)
            | SessionKind::AudioFixtures(_)
            | SessionKind::File(_)
            | SessionKind::HttpTest(_) => Err(StatusCode::NOT_FOUND),
        }
    }

    /// HEAD handler for session segments — returns `head_reported_segment_size` if configured.
    #[expect(clippy::significant_drop_tightening)]
    async fn session_segment_head(
        State(state): State<AppState>,
        Path((id, _filename)): Path<(String, String)>,
    ) -> Result<(StatusCode, [(header::HeaderName, String); 1]), StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        let size = match &session.kind {
            SessionKind::Hls(data) => data
                .config
                .head_reported_segment_size
                .unwrap_or(data.config.segment_size),
            SessionKind::FixedHls(_) | SessionKind::Abr(_) => 200_000,
            SessionKind::AudioFixtures(_) | SessionKind::File(_) | SessionKind::HttpTest(_) => {
                return Err(StatusCode::NOT_FOUND);
            }
        };
        Ok((StatusCode::OK, [(header::CONTENT_LENGTH, size.to_string())]))
    }

    #[expect(clippy::significant_drop_tightening)]
    async fn session_segment(
        State(state): State<AppState>,
        Path((id, filename)): Path<(String, String)>,
    ) -> Result<Vec<u8>, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        let (variant, segment) = parse_segment_filename(&filename).unwrap_or((0, 0));

        match &session.kind {
            SessionKind::FixedHls(_) => Ok(fixed_test_segment_data(variant, segment)),
            SessionKind::Hls(data) => {
                let delay_ms = eval_delay(&data.config.delay_rules, variant, segment);
                if delay_ms > 0 {
                    // Drop the read lock before sleeping to avoid holding it.
                    let delay = Duration::from_millis(delay_ms);
                    drop(sessions);
                    kithara_platform::time::sleep(delay).await;
                    let sessions = state.sessions.read().await;
                    let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
                    let SessionKind::Hls(data) = &session.kind else {
                        return Err(StatusCode::NOT_FOUND);
                    };
                    return serve_hls_segment(data, variant, segment);
                }
                serve_hls_segment(data, variant, segment)
            }
            SessionKind::Abr(data) => {
                let (delay, size) = if variant == 2 && segment == 0 {
                    (Duration::from_millis(data.config.segment0_delay_ms), 50_000)
                } else {
                    (Duration::from_millis(1), 200_000)
                };
                drop(sessions);
                Ok(abr_segment_data(variant, segment, delay, size).await)
            }
            SessionKind::AudioFixtures(_) | SessionKind::File(_) | SessionKind::HttpTest(_) => {
                Err(StatusCode::NOT_FOUND)
            }
        }
    }

    fn serve_hls_segment(
        data: &HlsData,
        variant: usize,
        segment: usize,
    ) -> Result<Vec<u8>, StatusCode> {
        let variant_data = data.segments.get(variant).ok_or(StatusCode::NOT_FOUND)?;
        let start = segment * data.config.segment_size;
        let end = (start + data.config.segment_size).min(variant_data.len());
        if start >= variant_data.len() {
            return Err(StatusCode::NOT_FOUND);
        }
        let plaintext = variant_data[start..end].to_vec();

        if let Some(ref enc) = data.config.encryption {
            let iv = derive_iv(enc, segment);
            Ok(encrypt_aes128_cbc(&plaintext, &data.key, &iv))
        } else {
            Ok(plaintext)
        }
    }

    #[expect(clippy::significant_drop_tightening)]
    async fn session_init(
        State(state): State<AppState>,
        Path((id, filename)): Path<(String, String)>,
    ) -> Result<Vec<u8>, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        // Parse variant from "v{v}_init.bin" or "v{v}.bin"
        let variant = parse_init_filename(&filename).unwrap_or(0);
        match &session.kind {
            SessionKind::FixedHls(_) => Ok(fixed_test_init_data(variant)),
            SessionKind::Hls(data) => data
                .inits
                .get(variant)
                .cloned()
                .ok_or(StatusCode::NOT_FOUND),
            SessionKind::Abr(_) => Ok(abr_init_data(variant)),
            SessionKind::AudioFixtures(_) | SessionKind::File(_) | SessionKind::HttpTest(_) => {
                Err(StatusCode::NOT_FOUND)
            }
        }
    }

    #[expect(clippy::significant_drop_tightening)]
    async fn session_key(
        State(state): State<AppState>,
        Path(id): Path<String>,
    ) -> Result<Vec<u8>, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        match &session.kind {
            SessionKind::FixedHls(_) => Ok(fixed_test_key_data()),
            SessionKind::Hls(data) => Ok(data.key.clone()),
            SessionKind::Abr(_)
            | SessionKind::AudioFixtures(_)
            | SessionKind::File(_)
            | SessionKind::HttpTest(_) => Err(StatusCode::NOT_FOUND),
        }
    }

    #[expect(clippy::significant_drop_tightening)]
    async fn session_aes_key(
        State(state): State<AppState>,
        Path(id): Path<String>,
    ) -> Result<Vec<u8>, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        match &session.kind {
            SessionKind::FixedHls(_) => Ok(fixed_aes128_key_bytes()),
            SessionKind::Hls(_)
            | SessionKind::Abr(_)
            | SessionKind::AudioFixtures(_)
            | SessionKind::File(_)
            | SessionKind::HttpTest(_) => Err(StatusCode::NOT_FOUND),
        }
    }

    #[expect(clippy::significant_drop_tightening)]
    async fn session_aes_seg(
        State(state): State<AppState>,
        Path(id): Path<String>,
    ) -> Result<Vec<u8>, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        match &session.kind {
            SessionKind::FixedHls(_) => Ok(fixed_aes128_ciphertext()),
            SessionKind::Hls(_)
            | SessionKind::Abr(_)
            | SessionKind::AudioFixtures(_)
            | SessionKind::File(_)
            | SessionKind::HttpTest(_) => Err(StatusCode::NOT_FOUND),
        }
    }

    // ── Audio fixtures route handler ─────────────────────────────────

    #[expect(clippy::significant_drop_tightening)]
    async fn session_audio_file(
        State(state): State<AppState>,
        Path((id, filename)): Path<(String, String)>,
    ) -> Result<Response<Body>, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        let SessionKind::AudioFixtures(data) = &session.kind else {
            return Err(StatusCode::NOT_FOUND);
        };
        let (bytes, content_type) = match filename.as_str() {
            "silence.wav" => (data.wav, "audio/wav"),
            "test.mp3" => (data.mp3, "audio/mpeg"),
            _ => return Err(StatusCode::NOT_FOUND),
        };
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", content_type)
            .header("Content-Length", bytes.len().to_string())
            .body(Body::from(bytes.to_vec()))
            .unwrap())
    }

    // ── File download route handlers ─────────────────────────────────

    #[expect(clippy::significant_drop_tightening)]
    async fn session_file_get(
        State(state): State<AppState>,
        Path((id, filename)): Path<(String, String)>,
        headers: HeaderMap,
    ) -> Result<Response<Body>, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        let SessionKind::File(data) = &session.kind else {
            return Err(StatusCode::NOT_FOUND);
        };
        let entry = data
            .config
            .files
            .iter()
            .find(|f| f.name == filename)
            .ok_or(StatusCode::NOT_FOUND)?;

        // Check for partial close on sequential (non-range) requests
        if let Some(ref pc) = data.config.partial_close
            && parse_range_header(&headers).is_none()
        {
            let count = data
                .sequential_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                // First sequential request: send partial data then close
                let truncated = &entry.data[..pc.close_after_bytes.min(entry.data.len())];
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, &entry.content_type)
                    .header(header::CONTENT_LENGTH, pc.total_size.to_string())
                    .header(header::ACCEPT_RANGES, "bytes")
                    .body(Body::from(truncated.to_vec()))
                    .unwrap());
            }
        }

        // Normal request with range support
        Ok(build_range_response(&entry.data, &headers, true))
    }

    #[expect(clippy::significant_drop_tightening)]
    async fn session_file_head(
        State(state): State<AppState>,
        Path((id, filename)): Path<(String, String)>,
    ) -> Result<Response<Body>, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        let SessionKind::File(data) = &session.kind else {
            return Err(StatusCode::NOT_FOUND);
        };
        let entry = data
            .config
            .files
            .iter()
            .find(|f| f.name == filename)
            .ok_or(StatusCode::NOT_FOUND)?;

        let size = data
            .config
            .partial_close
            .as_ref()
            .map_or(entry.data.len(), |pc| pc.total_size);

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, &entry.content_type)
            .header(header::CONTENT_LENGTH, size.to_string())
            .header(header::ACCEPT_RANGES, "bytes")
            .body(Body::empty())
            .unwrap())
    }

    // ── HTTP test route handler ─────────────────────────────────────

    async fn session_http_test(
        State(state): State<AppState>,
        Path((id, path)): Path<(String, String)>,
        headers: HeaderMap,
        method: axum::http::Method,
    ) -> Result<Response<Body>, StatusCode> {
        let sessions = state.sessions.read().await;
        let session = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        let SessionKind::HttpTest(data) = &session.kind else {
            return Err(StatusCode::NOT_FOUND);
        };

        let route_path = format!("/{path}");
        let route = data
            .config
            .routes
            .iter()
            .find(|r| r.path == route_path)
            .ok_or(StatusCode::NOT_FOUND)?;

        // Track request count
        let count = data
            .route_counts
            .get(&route_path)
            .map_or(0, |c| c.fetch_add(1, std::sync::atomic::Ordering::SeqCst));

        // Clone what we need before dropping the read lock
        let route = route.clone();
        drop(sessions);

        // Handle fail_first_n (for retry tests)
        if let Some(fail_n) = route.fail_first_n
            && count < fail_n
        {
            return Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap());
        }

        // Delay if configured
        if let Some(delay_ms) = route.delay_ms
            && delay_ms > 0
        {
            kithara_platform::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        let body_data = route.body.unwrap_or_default();

        // Range support
        if route.support_range && !body_data.is_empty() {
            return Ok(build_range_response(
                &body_data,
                &headers,
                method != axum::http::Method::HEAD,
            ));
        }

        let status = StatusCode::from_u16(route.status).unwrap_or(StatusCode::OK);
        let mut builder = Response::builder().status(status);
        for (key, value) in &route.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }
        if method == axum::http::Method::HEAD {
            builder = builder.header(header::CONTENT_LENGTH, body_data.len().to_string());
            Ok(builder.body(Body::empty()).unwrap())
        } else {
            Ok(builder.body(Body::from(body_data)).unwrap())
        }
    }

    // ── Static route handlers (backwards compat) ───────────────────

    async fn static_master_uniform() -> &'static str {
        "#EXTM3U\n\
         #EXT-X-VERSION:6\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1280000\n\
         playlist/v0.m3u8\n"
    }

    async fn static_master_jitter() -> &'static str {
        "#EXTM3U\n\
         #EXT-X-VERSION:6\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1280000\n\
         playlist/jitter.m3u8\n"
    }

    async fn static_media(State(state): State<AppState>, Path(filename): Path<String>) -> String {
        if filename == "jitter.m3u8" {
            static_media_playlist(&state.static_jitter, "j0_")
        } else {
            static_media_playlist(&state.static_uniform, "v0_")
        }
    }

    async fn static_segment_get(
        State(state): State<AppState>,
        Path(filename): Path<String>,
        headers: HeaderMap,
    ) -> Response<Body> {
        let data = if let Some(idx) = parse_static_uniform_index(&filename) {
            serve_static_segment(&state.static_uniform, idx)
        } else if let Some(idx) = parse_static_jitter_index(&filename) {
            serve_static_segment(&state.static_jitter, idx)
        } else {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        };
        if data.is_empty() {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        }
        build_range_response(&data, &headers, true)
    }

    async fn static_segment_head(
        State(state): State<AppState>,
        Path(filename): Path<String>,
        headers: HeaderMap,
    ) -> Response<Body> {
        let len = if let Some(idx) = parse_static_uniform_index(&filename) {
            state
                .static_uniform
                .segment_ranges
                .get(idx)
                .map_or(0, |(s, e)| e - s)
        } else if let Some(idx) = parse_static_jitter_index(&filename) {
            state
                .static_jitter
                .segment_ranges
                .get(idx)
                .map_or(0, |(s, e)| e - s)
        } else {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        };
        if len == 0 {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        }
        build_range_response(&vec![0u8; len], &headers, false)
    }

    fn serve_static_segment(fixture: &StaticFixture, segment: usize) -> Vec<u8> {
        let Some((start, end)) = fixture.segment_ranges.get(segment).copied() else {
            return Vec::new();
        };
        fixture.wav_data[start..end].to_vec()
    }

    // ── Parsing helpers ────────────────────────────────────────────

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
        // "v{v}_init.bin" or "v{v}.bin"
        if let Some(name) = filename.strip_suffix("_init.bin") {
            return name.strip_prefix('v')?.parse().ok();
        }
        if let Some(name) = filename.strip_suffix(".bin") {
            return name.strip_prefix('v')?.parse().ok();
        }
        None
    }

    fn parse_static_uniform_index(filename: &str) -> Option<usize> {
        filename
            .strip_suffix(".bin")?
            .strip_prefix("v0_")?
            .parse()
            .ok()
    }

    fn parse_static_jitter_index(filename: &str) -> Option<usize> {
        filename
            .strip_suffix(".bin")?
            .strip_prefix("j0_")?
            .parse()
            .ok()
    }

    // ── Session cleanup ────────────────────────────────────────────

    async fn cleanup_expired_sessions(sessions: Sessions) {
        loop {
            kithara_platform::time::sleep(Duration::from_secs(30)).await;
            let mut map = sessions.write().await;
            let now = Instant::now();
            map.retain(|_, session| {
                now.duration_since(session.created).as_secs() < SESSION_TTL_SECS
            });
        }
    }

    // ── Server entry point ─────────────────────────────────────────

    pub(crate) async fn run(port: u16) {
        let uniform_sizes = vec![UNIFORM_SEGMENT_SIZE; STATIC_SEGMENT_COUNT];
        let static_uniform = Arc::new(build_static_fixture(&uniform_sizes));
        let static_jitter = Arc::new(build_static_fixture(&jitter_segment_sizes()));

        let sessions: Sessions = Arc::new(RwLock::new(HashMap::new()));

        let state = AppState {
            sessions: Arc::clone(&sessions),
            static_uniform: Arc::clone(&static_uniform),
            static_jitter: Arc::clone(&static_jitter),
            port,
        };

        // Background cleanup task
        tokio::spawn(cleanup_expired_sessions(Arc::clone(&sessions)));

        let app = Router::new()
            // Health check
            .route("/health", get(health))
            // Session management API
            .route("/session/hls-fixed", post(create_fixed_hls_session))
            .route("/session/hls", post(create_hls_session))
            .route("/session/abr", post(create_abr_session))
            .route(
                "/session/audio-fixtures",
                post(create_audio_fixtures_session),
            )
            .route("/session/file", post(create_file_session))
            .route("/session/http-test", post(create_http_test_session))
            .route("/session/{id}", delete(delete_session_handler))
            // Session-based content routes
            .route("/s/{id}/master.m3u8", get(session_master))
            .route("/s/{id}/master-init.m3u8", get(session_master_init))
            .route(
                "/s/{id}/master-encrypted.m3u8",
                get(session_master_encrypted),
            )
            .route("/s/{id}/playlist/{filename}", get(session_playlist))
            .route("/s/{id}/{filename}", get(session_fixed_variant_playlist))
            .route(
                "/s/{id}/seg/{filename}",
                get(session_segment).head(session_segment_head),
            )
            .route("/s/{id}/init/{filename}", get(session_init))
            .route("/s/{id}/key.bin", get(session_key))
            .route("/s/{id}/aes/key.bin", get(session_aes_key))
            .route("/s/{id}/aes/seg0.bin", get(session_aes_seg))
            .route("/s/{id}/audio/{filename}", get(session_audio_file))
            .route(
                "/s/{id}/file/{filename}",
                get(session_file_get).head(session_file_head),
            )
            .route(
                "/s/{id}/http/{*path}",
                get(session_http_test)
                    .head(session_http_test)
                    .post(session_http_test),
            )
            // Static routes (backwards compatibility for WASM stress tests)
            .route("/master.m3u8", get(static_master_uniform))
            .route("/master-jitter.m3u8", get(static_master_jitter))
            .route("/playlist/{filename}", get(static_media))
            .route(
                "/seg/{filename}",
                get(static_segment_get).head(static_segment_head),
            )
            // Real asset routes (served from repository assets/ directory)
            .nest_service("/hls", ServeDir::new(assets_dir().join("hls")))
            .nest_service("/drm", ServeDir::new(assets_dir().join("drm")))
            .nest_service("/track.mp3", ServeDir::new(assets_dir().join("track.mp3")))
            .with_state(state)
            .layer(CorsLayer::permissive());

        let addr = format!("127.0.0.1:{port}");
        let listener = TcpListener::bind(&addr)
            .await
            .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));

        println!("Fixture server listening on http://{addr}");
        println!("Health: http://{addr}/health");
        println!("Static master (uniform): http://{addr}/master.m3u8");
        println!("Static master (jitter):  http://{addr}/master-jitter.m3u8");
        println!(
            "Session API: POST http://{addr}/session/hls-fixed | /session/hls | /session/abr | /session/audio-fixtures"
        );
        println!("Press Ctrl+C to stop");

        axum::serve(listener, app)
            .await
            .unwrap_or_else(|e| panic!("server error: {e}"));
    }
}
