#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! Sibling-fetch isolation when one HLS segment URL returns `text/html`.
//!
//! Spawns a custom axum server hosting a valid master + media playlist with
//! several segments. One segment URL is intentionally rigged to respond with
//! `Content-Type: text/html` (a CDN soft-error page). The test then drives a
//! [`Stream<Hls>`] read loop and inspects per-URL hit counters on the server
//! to prove that the html response on a single segment does **not** abort
//! sibling segment fetches inside the same `HlsPeer` epoch.
//!
//! The hypothesis under audit (see `feedback_*` memory entries):
//!     "When the Downloader receives one invalid response (e.g. text/html
//!      instead of media), it cascades-cancels every in-flight FetchCmd
//!      across every registered track."
//!
//! Cancel hierarchy (per audit of `crates/kithara-stream/src/dl/`):
//!   - Downloader-wide cancel (parent of every `peer_cancel`).
//!   - `peer_cancel` per registered `PeerHandle` (child of downloader cancel).
//!   - For each `FetchCmd` from `Peer::poll_next`, the Registry builds a
//!     `CancelGroup{peer_cancel.clone(), epoch_cancel}` (see
//!     `dl/registry.rs:130`). Every sibling in the same `poll_next` batch
//!     receives the SAME `epoch_cancel` instance (cloned in
//!     `kithara-hls/src/peer.rs:673`). Firing `epoch_cancel` therefore
//!     aborts every in-flight sibling — but `epoch_cancel.cancel()` only
//!     fires from `process_demand` on a new seek epoch. Validator failure
//!     in `dl/batch.rs:165` returns `Err` for one fetch only and never
//!     touches `peer_cancel` or `epoch_cancel`.
//!
//! Expected behaviour: `text/html` on segment 3 must not cancel siblings.
//! `HlsPeer::on_complete` for that fetch removes its in_flight slot and
//! rewinds the cursor onto seg 3; siblings 0..2 / 4..5 must still complete.

use std::{
    io::{Read, Seek, SeekFrom},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use kithara::{
    assets::StoreOptions,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::{TestTempDir, temp_dir};
use tokio::{net::TcpListener, task};
use tokio_util::sync::CancellationToken;
use url::Url;

struct Consts;
impl Consts {
    const NUM_SEGMENTS: usize = 6;
    const HTML_SEGMENT_INDEX: usize = 3;
    const SEGMENT_SIZE: usize = 64 * 1024;
}

#[derive(Clone)]
struct ServerState {
    /// Hits per segment index. `segment_hits[i]` increments every time the
    /// server receives a request for `/seg/v0_{i}.bin`.
    segment_hits: Arc<Vec<AtomicUsize>>,
    /// Hits on master / media playlist URLs. Useful for sanity checks.
    master_hits: Arc<AtomicUsize>,
    media_hits: Arc<AtomicUsize>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            segment_hits: Arc::new(
                (0..Consts::NUM_SEGMENTS)
                    .map(|_| AtomicUsize::new(0))
                    .collect(),
            ),
            master_hits: Arc::new(AtomicUsize::new(0)),
            media_hits: Arc::new(AtomicUsize::new(0)),
        }
    }
}

async fn master_handler(State(state): State<ServerState>) -> Response {
    state.master_hits.fetch_add(1, Ordering::Relaxed);
    let body = "#EXTM3U\n\
                #EXT-X-VERSION:3\n\
                #EXT-X-STREAM-INF:BANDWIDTH=128000\n\
                v0.m3u8\n";
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/vnd.apple.mpegurl".parse().unwrap(),
    );
    (StatusCode::OK, headers, body.to_string()).into_response()
}

async fn media_handler(State(state): State<ServerState>) -> Response {
    state.media_hits.fetch_add(1, Ordering::Relaxed);
    let mut body = String::from(
        "#EXTM3U\n\
         #EXT-X-VERSION:3\n\
         #EXT-X-TARGETDURATION:4\n\
         #EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n",
    );
    for i in 0..Consts::NUM_SEGMENTS {
        body.push_str(&format!("#EXTINF:4.0,\nseg/v0_{i}.bin\n"));
    }
    body.push_str("#EXT-X-ENDLIST\n");

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/vnd.apple.mpegurl".parse().unwrap(),
    );
    (StatusCode::OK, headers, body).into_response()
}

async fn segment_handler(State(state): State<ServerState>, Path(name): Path<String>) -> Response {
    // Parse "v0_{N}.bin"
    let idx_str = name
        .strip_prefix("v0_")
        .and_then(|s| s.strip_suffix(".bin"));
    let Some(idx_str) = idx_str else {
        return (StatusCode::NOT_FOUND, "bad name").into_response();
    };
    let Ok(idx) = idx_str.parse::<usize>() else {
        return (StatusCode::NOT_FOUND, "bad index").into_response();
    };
    if idx >= Consts::NUM_SEGMENTS {
        return (StatusCode::NOT_FOUND, "out of range").into_response();
    }
    state.segment_hits[idx].fetch_add(1, Ordering::Relaxed);

    if idx == Consts::HTML_SEGMENT_INDEX {
        // CDN soft-error page: 200 OK with text/html body.
        let html = "<html><body>503 Backend Error</body></html>";
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            "text/html; charset=utf-8".parse().unwrap(),
        );
        return (StatusCode::OK, headers, html.to_string()).into_response();
    }

    // Valid binary body. Bytes are arbitrary — Symphonia will not be able to
    // decode them, but the FETCH path completes and `on_complete` fires Ok.
    // The test asserts behaviour at the HTTP layer, not at the decoder.
    let body = vec![0xABu8; Consts::SEGMENT_SIZE];
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "video/mp2t".parse().unwrap());
    headers.insert(
        header::CONTENT_LENGTH,
        Consts::SEGMENT_SIZE.to_string().parse().unwrap(),
    );
    (StatusCode::OK, headers, body).into_response()
}

async fn start_server(state: ServerState) -> SocketAddr {
    let app = Router::new()
        .route("/master.m3u8", get(master_handler))
        .route("/v0.m3u8", get(media_handler))
        .route("/seg/{name}", get(segment_handler))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    task::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

/// One html-bodied segment (segment 3) must NOT prevent the Downloader from
/// fetching the surrounding segments (0..2 and 4..5). Per the audit: the
/// validator on `download_atomic_bytes` is only applied to playlists / DRM
/// keys, not to media segments. Even if one segment writes html bytes into
/// its `AssetResource`, sibling fetches in the same `HlsPeer::poll_next`
/// batch share `peer_cancel + epoch_cancel` and neither token fires from
/// `on_complete` failure paths (`crates/kithara-hls/src/peer.rs:624..676`).
///
/// Failure mode the test would reveal: if a future change wires the
/// segment writer / `on_complete` failure into `epoch_cancel.cancel()` or
/// into a wider abort, sibling segments would never be requested. That
/// would manifest as `segment_hits[i] == 0` for some `i != 3`.
#[kithara::test(tokio, timeout(Duration::from_secs(15)))]
async fn html_segment_does_not_cancel_sibling_fetches(temp_dir: TestTempDir) {
    let state = ServerState::new();
    let addr = start_server(state.clone()).await;

    let master = Url::parse(&format!("http://{addr}/master.m3u8")).expect("parse master url");

    let cancel = CancellationToken::new();
    let store = StoreOptions::new(temp_dir.path());
    let config = HlsConfig::new(master)
        .with_store(store)
        .with_cancel(cancel.clone());

    // Stream creation parses master + media playlists. Both return valid
    // bodies, so this must succeed.
    let stream = Stream::<Hls>::new(config)
        .await
        .expect("HLS stream creation must succeed with valid playlists");

    // Drive sibling fetches by walking the byte map. Each segment is
    // Consts::SEGMENT_SIZE bytes; reading at offset N*Consts::SEGMENT_SIZE forces the
    // scheduler to demand-load segment N. Use the synchronous `Read +
    // Seek` impl on a blocking thread to avoid blocking the runtime.
    let mut stream = stream;
    let hits_for_blocking = Arc::clone(&state.segment_hits);
    let _ = task::spawn_blocking(move || {
        let mut buf = vec![0u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(8);
        for seg in 0..Consts::NUM_SEGMENTS {
            // Skip the html-bodied segment from the seek list — its bytes
            // are not media and Symphonia would reject them. The server
            // still records a hit when the scheduler first attempts the
            // segment (before any decoder is involved).
            if seg == Consts::HTML_SEGMENT_INDEX {
                continue;
            }
            let offset = (seg * Consts::SEGMENT_SIZE) as u64;
            let _ = stream.seek(SeekFrom::Start(offset));
            let _ = stream.read(&mut buf);
            // Spin until the server records a hit (or the deadline passes)
            // to make the test deterministic.
            while hits_for_blocking[seg].load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    })
    .await;

    cancel.cancel();

    // Sanity: master + media playlists were fetched.
    assert!(
        state.master_hits.load(Ordering::Relaxed) >= 1,
        "master playlist must have been fetched at least once",
    );
    assert!(
        state.media_hits.load(Ordering::Relaxed) >= 1,
        "media playlist must have been fetched at least once",
    );

    // Core invariant: every non-html segment URL was requested at least
    // once. If the html response on segment {Consts::HTML_SEGMENT_INDEX} cascaded
    // cancellation across the peer / epoch tokens, the post-html siblings
    // (4, 5) would never reach the network — their counters would stay at
    // zero. The pre-html siblings (0, 1, 2) are below the cursor at the
    // time of the html response and so are an independent control sample.
    let snapshot: Vec<usize> = state
        .segment_hits
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .collect();

    let mut missing: Vec<usize> = Vec::new();
    for (idx, hits) in snapshot.iter().enumerate() {
        if idx == Consts::HTML_SEGMENT_INDEX {
            continue;
        }
        if *hits == 0 {
            missing.push(idx);
        }
    }

    assert!(
        missing.is_empty(),
        "sibling segment fetch cancelled when one sibling returned html: \
         missing segments {missing:?}, full hit map {snapshot:?} \
         (html segment is index {html_idx})",
        html_idx = Consts::HTML_SEGMENT_INDEX,
    );

    // The html-bodied segment was hit at least once: the scheduler tried
    // to fetch it (good — confirms the validator path is exercised).
    assert!(
        state.segment_hits[Consts::HTML_SEGMENT_INDEX].load(Ordering::Relaxed) >= 1,
        "rigged html segment was never requested — test fixture broken",
    );
}
