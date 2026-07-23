#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! A file gap-resume loop that makes zero progress must surface a terminal
//! `FileEvent::Error` within a bounded number of re-fetches. Models a host
//! that serves the head of a file and then blackholes every follow-up.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::get,
};
use bytes::Bytes;
use futures::stream;
use kithara::{
    events::{Event, EventBus, FileEvent},
    file::{File, FileConfig, FileSrc},
    net::{HttpClient, NetOptions, RetryPolicy},
    platform::{CancelToken, time::Duration},
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::{TestHttpServer, TestTempDir, kithara};

struct Consts;
impl Consts {
    /// Advertised full length; the stall leaves a large permanent gap.
    const TOTAL: usize = 1_024_000;
    /// Bytes the server actually delivers before going dark.
    const HEAD: usize = 64 * 1024;
    /// Short idle timeout so each dead fetch cycles quickly.
    const INACTIVITY: Duration = Duration::from_millis(300);
    /// Wall-clock ceiling for the terminal error. Generous against the
    /// per-cycle cost (idle timeout + one retry) so a pass is a real
    /// budget, not a lucky race.
    const ERROR_DEADLINE: Duration = Duration::from_secs(20);
}

#[derive(Clone)]
struct StallState {
    data: Arc<Vec<u8>>,
    gets: Arc<AtomicUsize>,
}

fn range_start(headers: &HeaderMap) -> usize {
    headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split('-').next())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
}

async fn serve_head_then_stall(State(state): State<StallState>, headers: HeaderMap) -> Response {
    state.gets.fetch_add(1, Ordering::SeqCst);
    let total = state.data.len();
    let start = range_start(&headers).min(total);
    let head_end = Consts::HEAD.clamp(start, total);
    let head = Bytes::copy_from_slice(&state.data[start..head_end]);

    let body = Body::from_stream(stream::unfold(Some(head), |chunk| async move {
        match chunk {
            Some(bytes) if !bytes.is_empty() => Some((Ok::<_, io::Error>(bytes), None)),
            _ => std::future::pending().await,
        }
    }));

    let mut builder = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .header(header::CONTENT_LENGTH, (total - start).to_string());
    builder = if start > 0 {
        builder.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{total}", total - 1),
        )
    } else {
        builder.status(StatusCode::OK)
    };
    builder.body(body).expect("stall response")
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(45)))]
async fn zero_progress_resume_loop_fails_terminally() {
    let data: Vec<u8> = (0..Consts::TOTAL).map(|i| (i % 256) as u8).collect();
    let gets = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/audio.mp3", get(serve_head_then_stall))
        .with_state(StallState {
            data: Arc::new(data),
            gets: Arc::clone(&gets),
        });
    let server = TestHttpServer::new(app).await;
    let url = server.url("/audio.mp3");

    let temp = TestTempDir::new();
    let cancel = CancelToken::never();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::builder()
                .inactivity_timeout(Consts::INACTIVITY)
                .retry_policy(RetryPolicy::new(
                    1,
                    Duration::from_millis(50),
                    Duration::from_millis(100),
                ))
                .build(),
            CancelToken::never(),
        ))
        .cancel(cancel.clone())
        .build(),
    );

    // Subscribe BEFORE the stream starts so the terminal publish is not raced.
    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();

    let config = FileConfig::for_src(FileSrc::Remote(url))
        .events(bus)
        .store(kithara_integration_tests::disk_asset_store(temp.path()))
        .cancel(cancel)
        .look_ahead_bytes(Consts::TOTAL as u64)
        .downloader(downloader)
        .build();
    let _stream = Stream::<File>::new(config)
        .await
        .expect("stream opens on the reachable head");

    let terminal = kithara::platform::time::timeout(Consts::ERROR_DEADLINE, async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(Event::File(FileEvent::Error { .. })) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
    })
    .await;

    assert!(
        matches!(terminal, Ok(true)),
        "zero-progress gap-resume must fail terminally within {:?} \
         (server saw {} fetches and the loop was still spinning)",
        Consts::ERROR_DEADLINE,
        gets.load(Ordering::SeqCst),
    );
    assert!(
        gets.load(Ordering::SeqCst) >= 2,
        "expected at least one zero-progress resume fetch, got {}",
        gets.load(Ordering::SeqCst),
    );
}
