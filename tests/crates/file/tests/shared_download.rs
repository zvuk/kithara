#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! Proves two consumers join one active `AssetStore` download and issue one GET.

use std::{
    convert::Infallible,
    io::{self, Read},
    sync::atomic::{AtomicUsize, Ordering},
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
    assets::{AssetStore, StorageBackend},
    file::{File, FileConfig},
    platform::{
        sync::Arc,
        time::Duration,
        tokio::{sync::watch, task::spawn_blocking},
    },
    stream::Stream,
};
use kithara_integration_tests::{
    TestHttpServer,
    bufpool_ext::{TestPools, pools},
};

const BODY: &[u8] = b"0123456789abcdefghijABCDEFGHIJ0123456789abcdefghijABCDEFGHIJ";

#[derive(Clone)]
struct CountState {
    arrived: watch::Sender<bool>,
    gets: Arc<AtomicUsize>,
    released: watch::Sender<bool>,
}

async fn serve_full(State(state): State<CountState>) -> Response {
    state.gets.fetch_add(1, Ordering::SeqCst);
    state.arrived.send_replace(true);
    let mut released = state.released.subscribe();
    let body = Body::from_stream(stream::once(async move {
        released
            .wait_for(|is_released| *is_released)
            .await
            .expect("test release sender remains alive");
        Ok::<Bytes, Infallible>(Bytes::from_static(BODY))
    }));
    Response::builder()
        .status(200)
        .header(header::CONTENT_LENGTH, BODY.len().to_string())
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .body(body)
        .expect("valid response")
}

async fn serve_range(State(state): State<CountState>, headers: HeaderMap) -> Response {
    state.gets.fetch_add(1, Ordering::SeqCst);
    state.arrived.send_replace(true);
    let value = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("bytes="))
        .expect("bounded request carries a byte range");
    let (start, end) = value.split_once('-').expect("range has start and end");
    let start = start.parse::<usize>().expect("numeric range start");
    let end = end.parse::<usize>().expect("numeric range end");
    let end = end.min(BODY.len() - 1);
    let mut released = state.released.subscribe();
    released
        .wait_for(|is_released| *is_released)
        .await
        .expect("test release sender remains alive");
    let bytes = &BODY[start..=end];
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", BODY.len()),
        )
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .body(Body::from(Bytes::copy_from_slice(bytes)))
        .expect("valid range response")
}

fn read_to_end(mut stream: Stream<File<TestPools>>) -> io::Result<Vec<u8>> {
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
    multi_thread,
    flash(false),
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(2)
)]
async fn follower_joins_active_download_without_second_get() {
    let gets = Arc::new(AtomicUsize::new(0));
    let (arrived, mut arrived_rx) = watch::channel(false);
    let (released, _released_rx) = watch::channel(false);
    let app = Router::new()
        .route("/audio.mp3", get(serve_full))
        .with_state(CountState {
            arrived,
            gets: Arc::clone(&gets),
            released: released.clone(),
        });
    let server = TestHttpServer::new(app).await;
    let url = server.url("/audio.mp3");
    let pools = pools();

    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .build();

    let waveform_cfg = FileConfig::for_src(url.clone().into())
        .store(store.clone())
        .pools(pools.clone())
        .build();
    let waveform = Stream::<File<TestPools>>::new(waveform_cfg)
        .await
        .expect("waveform stream");
    arrived_rx
        .wait_for(|has_arrived| *has_arrived)
        .await
        .expect("request handler remains alive");

    let player_cfg = FileConfig::for_src(url.into())
        .store(store)
        .pools(pools)
        .look_ahead_bytes(16)
        .build();
    let player = Stream::<File<TestPools>>::new(player_cfg)
        .await
        .expect("player stream");

    let waveform_read = spawn_blocking(move || read_to_end(waveform));
    let player_read = spawn_blocking(move || read_to_end(player));
    released.send_replace(true);
    let waveform_bytes = waveform_read
        .await
        .expect("waveform blocking task")
        .expect("whole-file read must complete");
    let player_bytes = player_read
        .await
        .expect("player blocking task")
        .expect("bounded read must complete");

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

#[kithara::test(
    tokio,
    multi_thread,
    flash(false),
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(2)
)]
async fn immediate_read_exceeds_zero_look_ahead_without_stalling() {
    let gets = Arc::new(AtomicUsize::new(0));
    let (arrived, _arrived_rx) = watch::channel(false);
    let (released, _released_rx) = watch::channel(true);
    let app = Router::new()
        .route("/audio.mp3", get(serve_range))
        .with_state(CountState {
            arrived,
            gets: Arc::clone(&gets),
            released,
        });
    let server = TestHttpServer::new(app).await;
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .build();
    let config = FileConfig::for_src(server.url("/audio.mp3").into())
        .store(store)
        .pools(pools)
        .look_ahead_bytes(0)
        .build();
    let stream = Stream::<File<TestPools>>::new(config)
        .await
        .expect("bounded stream");

    let bytes = spawn_blocking(move || read_to_end(stream))
        .await
        .expect("bounded read task")
        .expect("bounded read must complete");

    assert_eq!(bytes, BODY);
    assert!(
        gets.load(Ordering::SeqCst) > 1,
        "zero look-ahead must advance through multiple exact ranges"
    );
}
