#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use firewheel::dsp::filter::smoothing_filter::{DEFAULT_SETTLE_RATIO, DEFAULT_SMOOTH_SECONDS};
use kithara::{
    assets::AssetStore,
    host::HostConfig,
    platform::time::Duration,
    play::{
        PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig,
        ResourceSrc, SessionError,
    },
    queue::TrackSource,
    sync::SyncGroup,
    warp::WarpConfig,
};
use kithara_integration_tests::{
    TestServerHelper, kithara,
    offline::OfflineHostHarness,
    smoothing::{
        SmoothingCase, assert_step_is_ramped, consts, last_block_peak, observe, observe_until,
        peak, sine_queue,
    },
};
use kithara_test_fixtures::SignalAsset;

use crate::bufpool_ext::pools;

/// Every window the smoothing oracles read is decoded output.
///
/// A render the decoder cannot fill writes the frames it had and zero-fills the
/// rest, and the silence lands inside the window as a jump no ramp bound
/// allows: the oracle then reports an unsmoothed step the DSP never produced.
/// The fixture parks its reads instead, so the window its siblings measure
/// carries no block the decoder never produced.
#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn the_observation_window_carries_no_silent_block() {
    let (harness, _) = sine_queue(SmoothingCase { eq_layout: None }).await;

    let pcm = observe(&harness, consts::OBSERVE_BLOCKS).await;

    let block = consts::BLOCK_FRAMES * consts::CHANNELS;
    let quietest = pcm
        .chunks_exact(block)
        .map(peak)
        .fold(f32::INFINITY, f32::min);
    assert!(
        quietest > consts::AUDIBLE_PEAK,
        "the observation window carried a block at peak {quietest}; the window the step \
         oracles measure carries frames the decoder never produced"
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn deck_volume_step_is_ramped() {
    let (harness, _) = sine_queue(SmoothingCase { eq_layout: None }).await;
    let before = observe(&harness, consts::OBSERVE_BLOCKS).await;
    harness.run(|deck| deck.set_volume(0.0)).await;
    let (after, silent) = observe_until(&harness, |block| peak(block) == 0.0).await;
    assert!(
        silent,
        "volume 0 must reach silence within the settle window (last block peak {})",
        last_block_peak(&after)
    );
    assert_step_is_ramped(
        "deck volume 1→0",
        &before,
        &after,
        peak(&before),
        DEFAULT_SMOOTH_SECONDS,
        DEFAULT_SETTLE_RATIO,
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn queue_append_while_playing_does_not_wait_for_host() {
    let (harness, first_id) = sine_queue(SmoothingCase { eq_layout: None }).await;
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::WAV_SINE440_60S);
    let src = ResourceSrc::parse(url.as_str()).expect("valid signal fixture URL");
    let second = harness
        .control()
        .append(TrackSource::Config(Box::new(
            ResourceConfig::for_src(src)
                .store(AssetStore::builder(pools()).build())
                .build(),
        )))
        .expect("append while first track is playing");
    assert_ne!(second.as_u64(), first_id);
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn prepared_deck_preserves_play_pause_order() {
    let (harness, _) = sine_queue(SmoothingCase { eq_layout: None }).await;
    let deck = harness.control();
    deck.pause();
    deck.play();
    deck.pause();
    let (paused, silent) = observe_until(&harness, |block| peak(block) == 0.0).await;
    assert!(
        silent,
        "the last pause must reach silence: {}",
        last_block_peak(&paused)
    );
    deck.play();
    let (resumed, audible) = observe_until(&harness, |block| peak(block) > 0.1).await;
    assert!(
        audible,
        "resume must become audible: {}",
        last_block_peak(&resumed)
    );
    harness.close().await;
}

#[kithara::test(tokio)]
async fn failed_deck_preparation_releases_host_membership() {
    let region = pools();
    let sample_rate = NonZeroU32::new(consts::SAMPLE_RATE).expect("sample rate");
    let config = HostConfig::offline(region.clone())
        .sample_rate(sample_rate)
        .max_block_frames(NonZeroU32::new(consts::BLOCK_FRAMES as u32).expect("block size"))
        .build();
    let host = OfflineHostHarness::new(config).await.expect("offline host");
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let invalid = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(sample_rate)
            .worker(worker.clone())
            .warp(
                WarpConfig::builder()
                    .render_quantum_frames(NonZeroUsize::new(32).expect("quantum"))
                    .build(),
            )
            .response_budget_frames(NonZeroUsize::new(1).expect("budget"))
            .build(),
    );
    assert!(matches!(
        host.insert(invalid).await,
        Err(PlayError::Session(
            SessionError::ResponseBudgetExceeded { .. }
        ))
    ));
    host.with(|host| {
        assert!(host.topology().expect("host topology").members().is_empty());
        assert!(
            host.sample_rate()
                .expect("host sample rate")
                .measured
                .is_none(),
            "failed preparation must close an otherwise idle stream"
        );
    })
    .await;
    let valid = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(sample_rate)
            .worker(worker)
            .build(),
    );
    let deck = host
        .insert(valid)
        .await
        .expect("host can prepare the next deck");
    host.with(move |host| {
        assert!(
            host.sample_rate().expect("sample rate").measured.is_none(),
            "inserting an idle deck must not start the output stream"
        );
        deck.set_eq_gain(0, -6.0).expect("configure idle EQ");
        assert_eq!(deck.eq_gain(0), Some(-6.0));
        deck.play();
        assert!(host.sample_rate().expect("sample rate").measured.is_some());
        assert_eq!(deck.eq_gain(0), Some(-6.0));
        deck.pause();
    })
    .await;
    host.close().await;
}
