#![cfg(not(target_arch = "wasm32"))]

use firewheel::dsp::filter::smoothing_filter::{DEFAULT_SETTLE_RATIO, DEFAULT_SMOOTH_SECONDS};
use kithara::platform::time::Duration;
use kithara_integration_tests::{
    kithara,
    smoothing::{
        Consts, SmoothingCase, assert_step_is_ramped, last_block_peak, observe, observe_until,
        sine_queue,
    },
};
use kithara_test_fixtures::signal::peak;

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

    let pcm = observe(&harness, Consts::OBSERVE_BLOCKS).await;

    let block = Consts::BLOCK_FRAMES * Consts::CHANNELS;
    let quietest = pcm
        .chunks_exact(block)
        .map(peak)
        .fold(f32::INFINITY, f32::min);
    assert!(
        quietest > Consts::AUDIBLE_PEAK,
        "the observation window carried a block at peak {quietest}; the window the step \
         oracles measure carries frames the decoder never produced"
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn deck_volume_step_is_ramped() {
    let (harness, _) = sine_queue(SmoothingCase { eq_layout: None }).await;
    let before = observe(&harness, Consts::OBSERVE_BLOCKS).await;
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
