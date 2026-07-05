#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! End-to-end proof of the app-wide shared `AssetStore` dedup: two
//! concurrent consumers of one URL (a whole-file waveform analyzer and a
//! bounded player) sharing a single store cooperate on a single network
//! download. The CAS-winning consumer drives one GET; the loser reads the
//! shared bytes through the same `AssetResource`.

use std::{
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{Router, body::Body, extract::State, http::header, response::Response, routing::get};
use bytes::Bytes;
use kithara::{
    assets::{AssetStoreBuilder, StorageBackend},
    file::{File, FileConfig},
    platform::{time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::TestHttpServer;

/// Deterministic 60-byte body served in one response.
const BODY: &[u8] = b"0123456789abcdefghijABCDEFGHIJ0123456789abcdefghijABCDEFGHIJ";

#[derive(Clone)]
struct CountState {
    gets: Arc<AtomicUsize>,
}

/// Serve the whole body in a single 200 response, counting GETs.
async fn serve_full(State(state): State<CountState>) -> Response {
    state.gets.fetch_add(1, Ordering::SeqCst);
    Response::builder()
        .status(200)
        .header(header::CONTENT_LENGTH, BODY.len().to_string())
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .body(Body::from(Bytes::from_static(BODY)))
        .expect("valid response")
}

fn read_to_end(mut stream: Stream<File>) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
async fn shared_store_one_get() {
    let gets = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/audio.mp3", get(serve_full))
        .with_state(CountState {
            gets: Arc::clone(&gets),
        });
    let server = TestHttpServer::new(app).await;
    let url = server.url("/audio.mp3");

    let store = Arc::new(
        AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .build(),
    );

    // Whole-file consumer (waveform) attaches first → wins the producer
    // election and drives the single download.
    let waveform_cfg = FileConfig::for_src(url.clone().into())
        .asset_store(Arc::clone(&store))
        .build();
    let waveform = Stream::<File>::new(waveform_cfg)
        .await
        .expect("waveform stream");

    // Bounded consumer (player) of the same URL → loses the election and
    // reads the shared bytes instead of issuing its own download.
    let player_cfg = FileConfig::for_src(url.into())
        .asset_store(Arc::clone(&store))
        .look_ahead_bytes(16)
        .build();
    let player = Stream::<File>::new(player_cfg)
        .await
        .expect("player stream");

    let (waveform_bytes, player_bytes) = spawn_blocking(move || {
        let waveform_bytes = read_to_end(waveform);
        let player_bytes = read_to_end(player);
        (waveform_bytes, player_bytes)
    })
    .await
    .expect("blocking read task");
    let waveform_bytes = waveform_bytes.expect("whole-file read must complete");
    let player_bytes = player_bytes.expect("bounded read must complete");

    assert_eq!(
        waveform_bytes, BODY,
        "whole-file consumer must read the full body"
    );
    assert_eq!(
        player_bytes, BODY,
        "bounded consumer must read the full body from the shared resource"
    );
    assert_eq!(
        gets.load(Ordering::SeqCst),
        1,
        "two concurrent consumers of one URL must share a single network GET"
    );
}
