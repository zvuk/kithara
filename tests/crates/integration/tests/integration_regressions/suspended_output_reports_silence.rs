#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    events::TrackId,
    play::{InterruptionKind, Resource},
    signal::AudioSpec,
};
use kithara_integration_tests::offline::{
    OfflinePlayer, OfflinePlayerOptions, resource_from_reader,
};
use kithara_test_fixtures::integration_fixtures::constant_half;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const WARMUP_BLOCKS: usize = 8;

/// A platform interruption stops the output below the engine: the audio
/// callback is no longer invoked, so the RT processor cannot observe the
/// interruption, cannot report it, and leaves every value it publishes frozen
/// at whatever it wrote last. Rendering nothing here is the whole point — it is
/// what an interrupted output does, and the first render afterwards is the
/// output coming back.
#[kithara::test(tokio)]
async fn a_suspended_output_reports_silence_until_the_rt_processor_runs_again(
    constant_half: &'static [u8],
) {
    let harness = loaded_harness(constant_half).await;
    assert_eq!(harness.player().rate(), 1.0);
    assert!(harness.player().is_playing());

    harness
        .player()
        .notify_interruption(InterruptionKind::Began);
    assert_eq!(
        harness.player().rate(),
        0.0,
        "a suspended output publishes no rate of its own, and nothing is audible"
    );
    assert!(
        !harness.player().is_playing(),
        "playback cannot be playing while the platform holds the output"
    );

    // Ending the interruption is not the output coming back: the system hands
    // it over, the stream rebuild takes it, and until the processor runs the
    // last thing it published still describes an output that is gone.
    harness
        .player()
        .notify_interruption(InterruptionKind::Ended {
            should_resume: true,
        });
    assert_eq!(
        harness.player().rate(),
        0.0,
        "an interruption that has ended does not by itself drive the output"
    );

    let _ = harness.render(BLOCK_FRAMES).await;
    assert_eq!(
        harness.player().rate(),
        1.0,
        "one audio block past the interruption the processor speaks for itself"
    );

    harness.close().await;
}

async fn loaded_harness(constant_half: &'static [u8]) -> OfflinePlayer {
    let harness =
        OfflinePlayer::with_sample_rate(OfflinePlayerOptions::builder().build(), SAMPLE_RATE).await;
    harness
        .with_player(move |player| {
            player.insert(
                resource_from_reader(kithara::audio::mock::TestPcmReader::with_pcm(
                    AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
                    1.0,
                    constant_half,
                )),
                TrackId::allocate(),
                None,
            );
            player
                .select_item(0, kithara::play::SelectionPlayback::Play)
                .expect("select first queue item");
        })
        .await;

    for _ in 0..WARMUP_BLOCKS {
        let _ = harness.render(BLOCK_FRAMES).await;
        let _ = harness.tick_and_drain().await;
    }
    harness
}
