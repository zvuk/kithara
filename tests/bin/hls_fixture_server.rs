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
    use std::sync::Arc;

    use axum::{Router, extract::Path, routing::get};
    use tokio::net::TcpListener;
    use tower_http::cors::CorsLayer;

    pub(crate) const PORT: u16 = 3333;
    const SAMPLE_RATE: u32 = 44100;
    const CHANNELS: u16 = 2;
    const SEGMENT_SIZE: usize = 200_000;
    const SEGMENT_COUNT: usize = 100;
    const TOTAL_BYTES: usize = SEGMENT_COUNT * SEGMENT_SIZE;

    struct ServerState {
        wav_data: Vec<u8>,
        segment_duration_secs: f64,
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

    fn master_playlist() -> &'static str {
        "#EXTM3U\n\
         #EXT-X-VERSION:6\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1280000\n\
         playlist/v0.m3u8\n"
    }

    fn media_playlist(state: &ServerState) -> String {
        let dur = state.segment_duration_secs;
        let target_dur = dur.ceil() as u64;
        let mut pl = format!(
            "#EXTM3U\n\
             #EXT-X-VERSION:6\n\
             #EXT-X-TARGETDURATION:{target_dur}\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-PLAYLIST-TYPE:VOD\n",
        );
        for seg in 0..SEGMENT_COUNT {
            pl.push_str(&format!("#EXTINF:{dur:.1},\n../seg/v0_{seg}.bin\n"));
        }
        pl.push_str("#EXT-X-ENDLIST\n");
        pl
    }

    fn serve_segment(state: &ServerState, filename: &str) -> Vec<u8> {
        let segment = parse_segment_index(filename).unwrap_or(0);
        let start = segment * SEGMENT_SIZE;
        let end = (start + SEGMENT_SIZE).min(state.wav_data.len());
        if start >= state.wav_data.len() {
            return Vec::new();
        }
        state.wav_data[start..end].to_vec()
    }

    fn parse_segment_index(filename: &str) -> Option<usize> {
        let name = filename.strip_suffix(".bin")?;
        let name = name.strip_prefix("v0_")?;
        name.parse().ok()
    }

    pub(crate) async fn run() {
        let wav_data = create_saw_wav(TOTAL_BYTES);
        let segment_duration =
            SEGMENT_SIZE as f64 / (f64::from(SAMPLE_RATE) * f64::from(CHANNELS) * 2.0);

        let state = Arc::new(ServerState {
            wav_data,
            segment_duration_secs: segment_duration,
        });

        let st_master = Arc::clone(&state);
        let st_media = Arc::clone(&state);
        let st_seg = Arc::clone(&state);

        let app = Router::new()
            .route(
                "/master.m3u8",
                get(move || {
                    let _s = Arc::clone(&st_master);
                    async move { master_playlist() }
                }),
            )
            .route(
                "/playlist/{filename}",
                get(move |Path(_filename): Path<String>| {
                    let s = Arc::clone(&st_media);
                    async move { media_playlist(&s) }
                }),
            )
            .route(
                "/seg/{filename}",
                get(move |Path(filename): Path<String>| {
                    let s = Arc::clone(&st_seg);
                    async move { serve_segment(&s, &filename) }
                }),
            )
            .layer(CorsLayer::permissive());

        let addr = format!("127.0.0.1:{PORT}");
        let listener = TcpListener::bind(&addr)
            .await
            .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));

        println!("HLS fixture server listening on http://{addr}");
        println!("Master playlist: http://{addr}/master.m3u8");
        println!(
            "Segments: {SEGMENT_COUNT} x {SEGMENT_SIZE} bytes = {} MB",
            TOTAL_BYTES / 1_000_000
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
