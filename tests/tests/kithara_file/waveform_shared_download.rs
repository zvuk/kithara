//! End-to-end proof that a waveform analysis and a concurrent player of
//! the same URL, sharing one app-wide `AssetStore`, cooperate on a single
//! network download. This exercises the production
//! `kithara_app::waveform::TrackAnalysisRunner` (open + preload + shared
//! analysis-worker decode), not a bare `Resource`.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, body::Body, extract::State, http::header, response::Response, routing::get};
use bytes::Bytes;
use kithara::{
    assets::{AssetStoreBuilder, StorageBackend},
    audio::{Audio, AudioConfig, ChunkOutcome, PcmReader, analysis::BeatAnalysisConfig},
    file::{File, FileConfig, FileSrc},
    platform::{CancelToken, sync::Arc, time::Duration, tokio::task::spawn_blocking},
    prelude::ResourceConfig,
    stream::Stream,
};
use kithara_app::waveform::TrackAnalysisRunner;
use kithara_integration_tests::{TestHttpServer, create_test_wav};

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

/// Drain the player to EOF. The pipeline is built with `block_on_underrun`
/// (see the call site), so `next_chunk` PARKS on the engine-coordinated ring
/// underrun until the decode worker delivers the next chunk or signals EOF —
/// it never returns `Pending`. The park drives the virtual clock forward under
/// flash (no real-clock re-poll sleep), so the worker advances and the shared
/// WAV is decoded deterministically.
fn drain_to_eof(mut audio: Audio<Stream<File>>) -> bool {
    loop {
        match audio.next_chunk() {
            Ok(ChunkOutcome::Chunk(_)) => {}
            Ok(ChunkOutcome::Pending { .. }) => {}
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

    let store = Arc::new(
        AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .build(),
    );

    // Waveform analysis consumer (whole-file) of the shared store.
    let waveform_cfg = ResourceConfig::for_src(url.as_str())
        .expect("waveform url")
        .asset_store(Arc::clone(&store))
        .build();

    // Player consumer of the same URL through the same shared store. Built
    // with `block_on_underrun(true)` so the drain parks on the virtual clock
    // until the worker delivers, instead of sleep-polling on `Pending`.
    let player_cfg = AudioConfig::<File>::for_stream(
        FileConfig::for_src(FileSrc::Remote(url.clone()))
            .asset_store(Arc::clone(&store))
            .build(),
    )
    .block_on_underrun(true)
    .build();

    // Run both concurrently so they cooperate on one download.
    let master = CancelToken::never();
    let mut runner =
        TrackAnalysisRunner::new(&master, WAVEFORM_BUCKETS, BeatAnalysisConfig::default());
    let mut analysis_rx = runner.analyze(waveform_cfg);

    let mut player = Audio::<Stream<File>>::new(player_cfg)
        .await
        .expect("open player audio");
    player.preload().expect("player preload");
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
    assert!(
        (1..=WAVEFORM_BUCKETS).contains(&envelope.len()),
        "waveform analysis must produce native-resolution buckets capped by request, got {}",
        envelope.len()
    );
    assert_eq!(
        gets.load(Ordering::SeqCst),
        1,
        "a waveform analysis and a concurrent player of one URL must share a single network GET"
    );
}
