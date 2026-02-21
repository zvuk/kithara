//! Standalone HLS fixture server with CORS headers for browser-based WASM tests.
//!
//! Serves saw-tooth WAV data as HLS segments on a fixed port.
//!
//! ```bash
//! cargo run --bin hls_fixture_server
//! # → Listening on http://127.0.0.1:3333
//! # Master playlist: http://127.0.0.1:3333/master.m3u8
//! ```

// This binary is native-only. On wasm32, provide a no-op main.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "WAV header construction uses fixed-size fields; values are small and safe"
)]
mod server {
    use std::{ops::Range, sync::Arc};

    use axum::{
        Router,
        body::Body,
        extract::Path,
        http::{HeaderMap, Response, StatusCode, header},
        routing::get,
    };
    use tokio::net::TcpListener;
    use tower_http::cors::CorsLayer;

    pub(crate) const PORT: u16 = 3333;
    const SAMPLE_RATE: u32 = 44100;
    const CHANNELS: u16 = 2;
    const SEGMENT_COUNT: usize = 100;
    const UNIFORM_SEGMENT_SIZE: usize = 200_000;
    const JITTER_MIN_SEGMENT_SIZE: usize = 140_000;
    const JITTER_SIZE_SPAN: usize = 120_000;

    #[derive(Clone, Copy)]
    enum FixtureKind {
        Jitter,
        Uniform,
    }

    struct ServerState {
        segment_durations_secs: Vec<f64>,
        segment_ranges: Vec<(usize, usize)>,
        wav_data: Vec<u8>,
    }

    fn create_saw_wav(total_bytes: usize) -> Vec<u8> {
        const SAW_PERIOD: usize = 65536;
        let bytes_per_sample: u16 = 2;
        let bytes_per_frame = CHANNELS as usize * bytes_per_sample as usize;
        let header_size = 44usize;
        let data_size = total_bytes - header_size;
        let frame_count = data_size / bytes_per_frame;
        let data_size = (frame_count * bytes_per_frame) as u32;
        let file_size = 36 + data_size;

        let mut wav = Vec::with_capacity(total_bytes);

        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&file_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&CHANNELS.to_le_bytes());
        wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        let byte_rate = SAMPLE_RATE * CHANNELS as u32 * bytes_per_sample as u32;
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align = CHANNELS * bytes_per_sample;
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());

        for i in 0..frame_count {
            let sample = ((i % SAW_PERIOD) as i32 - 32768) as i16;
            for _ in 0..CHANNELS {
                wav.extend_from_slice(&sample.to_le_bytes());
            }
        }

        wav.resize(total_bytes, 0);
        wav
    }

    fn master_playlist_uniform() -> &'static str {
        "#EXTM3U\n\
         #EXT-X-VERSION:6\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1280000\n\
         playlist/v0.m3u8\n"
    }

    fn master_playlist_jitter() -> &'static str {
        "#EXTM3U\n\
         #EXT-X-VERSION:6\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1280000\n\
         playlist/jitter.m3u8\n"
    }

    fn media_playlist(state: &ServerState, prefix: &str) -> String {
        let target_dur = state
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
        for (seg, dur) in state.segment_durations_secs.iter().copied().enumerate() {
            pl.push_str(&format!("#EXTINF:{dur:.3},\n../seg/{prefix}{seg}.bin\n"));
        }
        pl.push_str("#EXT-X-ENDLIST\n");
        pl
    }

    fn serve_segment(state: &ServerState, segment: usize) -> Vec<u8> {
        let Some((start, end)) = state.segment_ranges.get(segment).copied() else {
            return Vec::new();
        };
        state.wav_data[start..end].to_vec()
    }

    fn segment_size(state: &ServerState, segment: usize) -> Option<usize> {
        state
            .segment_ranges
            .get(segment)
            .map(|(start, end)| end - start)
    }

    fn parse_uniform_segment_index(filename: &str) -> Option<usize> {
        let name = filename.strip_suffix(".bin")?;
        let name = name.strip_prefix("v0_")?;
        name.parse().ok()
    }

    fn parse_jitter_segment_index(filename: &str) -> Option<usize> {
        let name = filename.strip_suffix(".bin")?;
        let name = name.strip_prefix("j0_")?;
        name.parse().ok()
    }

    fn parse_segment_request(filename: &str) -> Option<(FixtureKind, usize)> {
        if let Some(idx) = parse_uniform_segment_index(filename) {
            return Some((FixtureKind::Uniform, idx));
        }
        parse_jitter_segment_index(filename).map(|idx| (FixtureKind::Jitter, idx))
    }

    fn parse_range_header(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
        let value = headers.get(header::RANGE)?;
        let value = value.to_str().ok()?.trim();
        let range = value.strip_prefix("bytes=")?;
        let mut parts = range.splitn(2, '-');
        let start_str = parts.next()?.trim();
        let end_str = parts.next()?.trim();
        let start = start_str.parse::<u64>().ok()?;
        let end = if end_str.is_empty() {
            None
        } else {
            Some(end_str.parse::<u64>().ok()?.saturating_add(1))
        };
        Some((start, end))
    }

    fn range_not_satisfiable(total: usize) -> Response<Body> {
        Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_RANGE, format!("bytes */{total}"))
            .body(Body::empty())
            .expect("valid range-not-satisfiable response")
    }

    #[expect(
        clippy::result_large_err,
        reason = "test binary; Response used as error for early return"
    )]
    fn select_range(
        total_len: usize,
        range: Option<(u64, Option<u64>)>,
    ) -> Result<(Range<usize>, StatusCode, Option<String>), Response<Body>> {
        if let Some((start, end_opt)) = range {
            if start >= total_len as u64 {
                return Err(range_not_satisfiable(total_len));
            }
            let end = end_opt.unwrap_or(total_len as u64).min(total_len as u64);
            if start >= end {
                return Err(range_not_satisfiable(total_len));
            }
            let status = if start == 0 && end == total_len as u64 {
                StatusCode::OK
            } else {
                StatusCode::PARTIAL_CONTENT
            };
            let content_range = if status == StatusCode::PARTIAL_CONTENT {
                Some(format!(
                    "bytes {}-{}/{}",
                    start,
                    end.saturating_sub(1),
                    total_len
                ))
            } else {
                None
            };
            Ok(((start as usize)..(end as usize), status, content_range))
        } else {
            Ok(((0..total_len), StatusCode::OK, None))
        }
    }

    fn build_segment_response(
        data: &[u8],
        headers: &HeaderMap,
        include_body: bool,
    ) -> Response<Body> {
        let total = data.len();
        match select_range(total, parse_range_header(headers)) {
            Ok((range, status, content_range)) => {
                let mut builder = Response::builder().status(status);
                builder = builder
                    .header(
                        header::CONTENT_LENGTH,
                        (range.end - range.start).to_string(),
                    )
                    .header(header::ACCEPT_RANGES, "bytes");
                if let Some(content_range_header) = content_range {
                    builder = builder.header(header::CONTENT_RANGE, content_range_header);
                }
                let body = if include_body {
                    Body::from(data[range].to_vec())
                } else {
                    Body::empty()
                };
                builder.body(body).expect("valid segment response")
            }
            Err(resp) => resp,
        }
    }

    fn not_found_response() -> Response<Body> {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("valid not-found response")
    }

    fn jitter_segment_sizes() -> Vec<usize> {
        (0..SEGMENT_COUNT)
            .map(|i| JITTER_MIN_SEGMENT_SIZE + ((i * 7919) % JITTER_SIZE_SPAN))
            .collect()
    }

    fn build_state(segment_sizes: &[usize]) -> ServerState {
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

        ServerState {
            segment_durations_secs,
            segment_ranges,
            wav_data,
        }
    }

    pub(crate) async fn run() {
        let uniform_sizes = vec![UNIFORM_SEGMENT_SIZE; SEGMENT_COUNT];
        let uniform_state = Arc::new(build_state(&uniform_sizes));
        let jitter_state = Arc::new(build_state(&jitter_segment_sizes()));

        let st_master_uniform = Arc::clone(&uniform_state);
        let st_master_jitter = Arc::clone(&jitter_state);
        let st_media_uniform = Arc::clone(&uniform_state);
        let st_media_jitter = Arc::clone(&jitter_state);
        let st_seg_uniform_get = Arc::clone(&uniform_state);
        let st_seg_jitter_get = Arc::clone(&jitter_state);
        let st_seg_uniform_head = Arc::clone(&uniform_state);
        let st_seg_jitter_head = Arc::clone(&jitter_state);

        let app = Router::new()
            .route(
                "/master.m3u8",
                get(move || {
                    let _s = Arc::clone(&st_master_uniform);
                    async move { master_playlist_uniform() }
                }),
            )
            .route(
                "/master-jitter.m3u8",
                get(move || {
                    let _s = Arc::clone(&st_master_jitter);
                    async move { master_playlist_jitter() }
                }),
            )
            .route(
                "/playlist/{filename}",
                get(move |Path(filename): Path<String>| {
                    let uniform = Arc::clone(&st_media_uniform);
                    let jitter = Arc::clone(&st_media_jitter);
                    async move {
                        if filename == "jitter.m3u8" {
                            media_playlist(&jitter, "j0_")
                        } else {
                            media_playlist(&uniform, "v0_")
                        }
                    }
                }),
            )
            .route(
                "/seg/{filename}",
                get(move |Path(filename): Path<String>, headers: HeaderMap| {
                    let uniform = Arc::clone(&st_seg_uniform_get);
                    let jitter = Arc::clone(&st_seg_jitter_get);
                    async move {
                        let data = match parse_segment_request(&filename) {
                            Some((FixtureKind::Uniform, seg)) => serve_segment(&uniform, seg),
                            Some((FixtureKind::Jitter, seg)) => serve_segment(&jitter, seg),
                            None => return not_found_response(),
                        };
                        if data.is_empty() {
                            return not_found_response();
                        }
                        build_segment_response(&data, &headers, true)
                    }
                })
                .head(move |Path(filename): Path<String>, headers: HeaderMap| {
                    let uniform = Arc::clone(&st_seg_uniform_head);
                    let jitter = Arc::clone(&st_seg_jitter_head);
                    async move {
                        let data_len = match parse_segment_request(&filename) {
                            Some((FixtureKind::Uniform, seg)) => {
                                segment_size(&uniform, seg).unwrap_or(0)
                            }
                            Some((FixtureKind::Jitter, seg)) => {
                                segment_size(&jitter, seg).unwrap_or(0)
                            }
                            None => return not_found_response(),
                        };
                        if data_len == 0 {
                            return not_found_response();
                        }
                        build_segment_response(&vec![0u8; data_len], &headers, false)
                    }
                }),
            )
            .layer(CorsLayer::permissive());

        let addr = format!("127.0.0.1:{PORT}");
        let listener = TcpListener::bind(&addr)
            .await
            .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));

        println!("HLS fixture server listening on http://{addr}");
        println!("Master playlist (uniform): http://{addr}/master.m3u8");
        println!("Master playlist (jitter):  http://{addr}/master-jitter.m3u8");
        println!(
            "Uniform segments: {SEGMENT_COUNT} x {UNIFORM_SEGMENT_SIZE} bytes = {} MB",
            uniform_sizes.iter().sum::<usize>() / 1_000_000
        );
        println!(
            "Jitter segments:  {SEGMENT_COUNT} variable bytes = {} MB",
            jitter_state.wav_data.len() / 1_000_000
        );
        println!("Press Ctrl+C to stop");

        axum::serve(listener, app)
            .await
            .unwrap_or_else(|e| panic!("server error: {e}"));
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    server::run().await;
}
