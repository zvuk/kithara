//! End-to-end proof that a waveform analysis and a concurrent player of
//! the same URL, sharing one app-wide `AssetStore`, cooperate on a single
//! network download. This exercises the production
//! `kithara_app::waveform::TrackAnalysisRunner` (open + preload + shared
//! analysis-worker decode), not a bare `Resource`.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Router, body::Body, extract::State, http::header, response::Response, routing::get};
use bytes::Bytes;
use kithara::{
    assets::AssetStoreBuilder,
    audio::ChunkOutcome,
    prelude::{Resource, ResourceConfig},
};
use kithara_app::waveform::TrackAnalysisRunner;
use kithara_integration_tests::{TestHttpServer, create_test_wav};
use kithara_platform::{CancellationToken, tokio::task::spawn_blocking};

const WAVEFORM_BUCKETS: usize = 100;

#[derive(Clone)]
struct CountState {
    gets: Arc<AtomicUsize>,
    wav: Arc<Vec<u8>>,
}

/// Serve the whole WAV in a single 200 response, counting GETs.
async fn serve_wav(State(state): State<CountState>) -> Response {
    state.gets.fetch_add(1, Ordering::SeqCst);
    Response::builder()
        .status(200)
        .header(header::CONTENT_LENGTH, state.wav.len().to_string())
        .header(header::CONTENT_TYPE, "audio/wav")
        .body(Body::from(Bytes::from((*state.wav).clone())))
        .expect("valid response")
}

fn drain_to_eof(mut resource: Resource) -> bool {
    loop {
        match resource.next_chunk() {
            Ok(ChunkOutcome::Chunk(_)) => {}
            Ok(ChunkOutcome::Pending { .. }) => std::thread::sleep(Duration::from_millis(2)),
            Ok(ChunkOutcome::Eof { .. }) => return true,
            Err(_) => return false,
        }
    }
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(2)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
async fn waveform_and_player_share_one_get() {
    // 1s stereo WAV.
    let wav = Arc::new(create_test_wav(44_100, 44_100, 2));
    let gets = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/audio.wav", get(serve_wav))
        .with_state(CountState {
            gets: Arc::clone(&gets),
            wav: Arc::clone(&wav),
        });
    let server = TestHttpServer::new(app).await;
    let url = server.url("/audio.wav");

    let store = Arc::new(AssetStoreBuilder::new().ephemeral(true).build());

    // Waveform analysis consumer (whole-file) of the shared store.
    let waveform_cfg = ResourceConfig::for_src(url.as_str())
        .expect("waveform url")
        .file_asset_store(Arc::clone(&store))
        .build();

    // Player consumer of the same URL through the same shared store.
    let player_cfg = ResourceConfig::for_src(url.as_str())
        .expect("player url")
        .file_asset_store(Arc::clone(&store))
        .build();

    // Run both concurrently so they cooperate on one download.
    let master = CancellationToken::default();
    let mut runner = TrackAnalysisRunner::new(&master, WAVEFORM_BUCKETS);
    let mut analysis_rx = runner.analyze(waveform_cfg);

    let mut player = Resource::new(player_cfg)
        .await
        .expect("open player resource");
    player.preload().await.expect("player preload");
    let player_drain = spawn_blocking(move || drain_to_eof(player));

    analysis_rx
        .changed()
        .await
        .expect("analysis must produce a result");
    let envelope = analysis_rx
        .borrow()
        .clone()
        .expect("analysis result present")
        .waveform
        .expect("waveform analyzer fills its slot");
    let player_ok = player_drain.await.expect("player drain task");

    assert!(player_ok, "player must decode the shared WAV to EOF");
    assert_eq!(
        envelope.len(),
        WAVEFORM_BUCKETS,
        "waveform analysis must produce one value per bucket"
    );
    assert_eq!(
        gets.load(Ordering::SeqCst),
        1,
        "a waveform analysis and a concurrent player of one URL must share a single network GET"
    );
}
