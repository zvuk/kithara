#![cfg(not(target_arch = "wasm32"))]

//! Regression contract for forward playback at a withheld HLS segment.
//!
//! The decoder must wait for the real segment gate and then resume after the
//! body arrives. The test observes only the public read outcome and the test
//! server's real request gate; it does not add an observation path to audio.

use std::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioRead, ReadOutcome},
    hls::{AbrMode, Hls, HlsConfig},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, Instant},
        tokio,
        tokio::task::spawn_blocking,
    },
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::{
    SegmentGateHandle,
    bufpool_ext::{TestPools, pools},
    hls_server::{HlsTestServer, HlsTestServerConfig},
};
use kithara_test_fixtures::hls_fixtures::{hls_pcm_boundary, hls_stream_header};
use tracing::info;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
/// Small segments so the boundary is reached after only a few reads.
const SEGMENT_SIZE: usize = 32_768;
const SEGMENT_COUNT: usize = 8;
/// The segment whose body is withheld; forward playback parks at its start.
const GATED_SEGMENT: usize = 4;

#[kithara::fixture]
async fn gated_audio(
    hls_stream_header: Vec<u8>,
    hls_pcm_boundary: Vec<u8>,
) -> (HlsTestServer, SegmentGateHandle) {
    let init_segment = Arc::new(hls_stream_header);
    let pcm = Arc::new(hls_pcm_boundary);
    let segment_duration = SEGMENT_SIZE as f64
        / (f64::from(SAMPLE_RATE) * f64::from(CHANNELS) * size_of::<i16>() as f64);
    let config = HlsTestServerConfig {
        variant_count: 1,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![Arc::clone(&pcm)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![1_000_000]),
        ..Default::default()
    };

    // Withhold the BODY of GATED_SEGMENT; its HEAD (size) stays open so the
    // up-front layout is complete and the worker reaches the boundary.
    HlsTestServer::with_segment_gate(config, 0, GATED_SEGMENT).await
}

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    tracing("kithara_audio=info,kithara_hls=info")
)]
async fn forward_into_withheld_segment_waits_and_resumes(
    #[future(awt)] gated_audio: (HlsTestServer, SegmentGateHandle),
) {
    let (server, gate) = gated_audio;
    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(SEGMENT_COUNT + 10).expect("nonzero"))
        .build();
    let hls_config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(store)
        .pools(pools)
        .cancel(cancel)
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let wav_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let audio_config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .media_info(wav_info)
        .build();

    let mut audio = worker
        .open(audio_config)
        .await
        .expect("audio creation (segment 0 not withheld)");

    let spec = audio.spec();
    let read_samples = spec.channels as usize * 512;

    // Set once decoding reaches the withheld boundary.
    let at_boundary = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));

    // Release only after the real segment request and decoding boundary.
    let release_gate = gate.clone();
    let release_at_boundary = Arc::clone(&at_boundary);
    let release_flag = Arc::clone(&released);
    let releaser = tokio::task::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if release_gate.requested() > 0 && release_at_boundary.load(Ordering::Acquire) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "decode never parked at the withheld boundary within budget"
            );
            time::sleep(Duration::from_millis(5)).await;
        }

        release_flag.store(true, Ordering::Release);
        release_gate.release();
    });

    let decode_at_boundary = Arc::clone(&at_boundary);
    let decode_released = Arc::clone(&released);
    let decode = spawn_blocking(move || -> (u64, u64) {
        let mut buf = vec![0.0f32; read_samples];
        let mut frames_before = 0u64;
        let mut frames_after = 0u64;

        // Phase 1: drive forward (blocking reads — the offline-pull contract)
        // until the gate has been released. Before release, reads consume the
        // ready segments 0..GATED_SEGMENT, then block at the withheld
        // boundary. The moment a read blocks (consumes all ready data and the
        // worker can't fill the ring), signal the boundary so the releaser
        // can release the real segment body.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if decode_released.load(Ordering::Acquire) {
                break;
            }
            // Signal "at boundary" once we've decoded a full segment's worth —
            // the worker is now past the ready segments and about to (or
            // already) block on the withheld one.
            if frames_before >= (GATED_SEGMENT as u64) * (SEGMENT_SIZE as u64) / 4 {
                decode_at_boundary.store(true, Ordering::Release);
            }
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => frames_before += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => {
                    decode_at_boundary.store(true, Ordering::Release);
                }
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("read error before release: {e}"),
            }
            assert!(
                Instant::now() < deadline,
                "decode stalled before the gate released within budget"
            );
        }

        // Phase 2: gate released — playback must resume past the boundary.
        let resume_deadline = Instant::now() + Duration::from_secs(15);
        while frames_after < 4_096 {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => frames_after += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => std::thread::yield_now(),
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("read error after release: {e}"),
            }
            assert!(
                Instant::now() < resume_deadline,
                "playback did not resume within budget after releasing the body"
            );
        }

        (frames_before, frames_after)
    });

    releaser.await.expect("releaser joins");
    let (frames_before, frames_after) = decode.await.expect("decode task joins");

    info!(frames_before, frames_after, "F5 forward-withhold result");

    assert!(
        frames_before > 0,
        "expected some frames decoded before the withheld boundary"
    );

    assert!(
        frames_after > 0,
        "playback did not resume after releasing the withheld body"
    );

    drop(server);
}
