#![cfg(not(target_arch = "wasm32"))]

use kithara::platform::time::Duration;
use kithara_integration_tests::{
    kithara,
    smoothing::{
        Consts, SmoothingCase, assert_step_is_ramped, last_block_peak, layout, observe,
        observe_until, sine_queue,
    },
};
use kithara_test_fixtures::signal::peak;

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn eq_gain_step_is_ramped() {
    let (harness, _) = sine_queue(SmoothingCase {
        eq_layout: Some(Consts::THREE_BAND),
    })
    .await;
    let before = observe(&harness, Consts::OBSERVE_BLOCKS).await;
    let before_peak = peak(&before);
    harness
        .run(|deck| deck.set_eq_gain(1, -24.0))
        .await
        .expect("band 1 exists");
    let (after, arrived) = observe_until(&harness, |block| peak(block) < before_peak * 0.75).await;
    assert!(
        arrived,
        "band 1 at -24 dB takes a share of the tone within the settle window (last block peak {} \
         vs {before_peak})",
        last_block_peak(&after)
    );
    assert_step_is_ramped(
        "eq band 1 0→-24 dB",
        &before,
        &after,
        before_peak,
        Consts::EQ_SMOOTH_SECONDS,
        Consts::EQ_SETTLE_RATIO,
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn eq_layout_switch_is_crossed_over() {
    let (harness, _) = sine_queue(SmoothingCase {
        eq_layout: Some(Consts::THREE_BAND),
    })
    .await;
    let before = observe(&harness, Consts::OBSERVE_BLOCKS).await;
    let before_peak = peak(&before);
    harness
        .run(|deck| deck.set_eq_layout(layout(Consts::FOUR_BAND)))
        .await
        .expect("layout accepted");
    let (after, arrived) = observe_until(&harness, |block| peak(block) < before_peak * 0.5).await;
    assert!(
        arrived,
        "the new layout is audible: a -24 dB band on the tone more than halves its peak within \
         the settle window (last block peak {} vs {before_peak})",
        last_block_peak(&after)
    );
    assert_step_is_ramped(
        "eq layout 3 bands at 0 dB → 4 bands with 440 Hz at -24 dB",
        &before,
        &after,
        before_peak,
        Consts::EQ_SMOOTH_SECONDS,
        Consts::EQ_SETTLE_RATIO,
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn eq_layout_change_during_crossover_stays_continuous() {
    let (harness, _) = sine_queue(SmoothingCase {
        eq_layout: Some(Consts::THREE_BAND),
    })
    .await;
    let before = observe(&harness, Consts::OBSERVE_BLOCKS).await;
    harness
        .run(|deck| deck.set_eq_layout(layout(Consts::FOUR_BAND)))
        .await
        .expect("first layout accepted");
    let mut after = observe(&harness, 1).await;
    harness
        .run(|deck| deck.set_eq_layout(layout(Consts::THREE_BAND)))
        .await
        .expect("second layout accepted");
    after.extend(observe(&harness, Consts::OBSERVE_BLOCKS).await);
    assert!(
        last_block_peak(&after) > peak(&before) * 0.95,
        "the final unity layout must become audible"
    );
    assert_step_is_ramped(
        "layout replaced during crossover",
        &before,
        &after,
        peak(&before),
        Consts::EQ_SMOOTH_SECONDS,
        Consts::EQ_SETTLE_RATIO,
    );
    harness.close().await;
}
