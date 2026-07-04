#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use kithara::{
    audio::{Audio, AudioConfig},
    bufpool::{BytePool, PcmPool},
    platform::time::Duration,
    play::Resource,
    stream::Stream,
};
use kithara_integration_tests::{
    create_test_wav, kithara,
    memory_source::{MemStream, MemStreamConfig, MemorySource},
};

use super::offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const CHANNELS: u16 = 2;
// 440 Hz sine fixture. At 2x bend the transport reads the source twice as fast,
// so the rendered output is pitched up to ~880 Hz — twice the zero crossings.
const FIXTURE_FRAMES: usize = SAMPLE_RATE as usize * 2;
const WARMUP_BLOCKS: usize = 4;
const WINDOW_BLOCKS: usize = 16;

fn wav_config() -> AudioConfig<MemStream> {
    let wav = create_test_wav(FIXTURE_FRAMES, SAMPLE_RATE, CHANNELS);
    let source = MemorySource::new(wav);
    AudioConfig::<MemStream>::for_stream(MemStreamConfig {
        source: Some(source),
        event_bus: None,
    })
    .byte_pool(BytePool::default())
    .pcm_pool(PcmPool::default())
    .hint("wav".to_string())
    .build()
}

async fn audio_resource(src: &'static str) -> Resource {
    let audio = Audio::<Stream<MemStream>>::new(wav_config())
        .await
        .expect("audio construction");
    Resource::from_reader(audio, Some(Arc::from(src)))
}

/// Count left-channel zero crossings over `blocks` rendered output blocks,
/// after discarding a warm-up prefix. A higher transport rate pitches the
/// source up, so the crossing count scales with the effective playback rate.
fn left_zero_crossings(harness: &OfflinePlayerHarness) -> usize {
    for _ in 0..WARMUP_BLOCKS {
        let _ = harness.render(BLOCK_FRAMES);
        let _ = harness.tick_and_drain();
    }

    let mut crossings = 0usize;
    let mut prev: Option<f32> = None;
    for _ in 0..WINDOW_BLOCKS {
        let block = harness.render(BLOCK_FRAMES);
        let _ = harness.tick_and_drain();
        for frame in block.chunks_exact(usize::from(CHANNELS)) {
            let sample = frame[0];
            if let Some(previous) = prev {
                if (previous < 0.0) != (sample < 0.0) {
                    crossings += 1;
                }
            }
            prev = Some(sample);
        }
    }
    crossings
}

/// Play the fixture at the given transport bend (applied before the item loads
/// so it is live from the first block) and return the left-channel zero
/// crossings over the measurement window.
async fn crossings_at_bend(bend: Option<f32>) -> usize {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    );
    harness
        .player()
        .insert(audio_resource("pitch-bend.wav").await, None, None);
    if let Some(bend) = bend {
        harness.player().set_pitch_bend(bend);
    }
    harness
        .player()
        .select_item(0, true)
        .expect("select pitch-bend fixture");
    left_zero_crossings(&harness)
}

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(20)))]
async fn pitch_bend_pitches_up_transport_output() {
    let unity = crossings_at_bend(None).await;
    let bent = crossings_at_bend(Some(2.0)).await;

    assert!(
        unity > 0,
        "unity playback must produce a tone before comparing bend; unity={unity}"
    );
    assert!(
        (bent as f64) > (unity as f64) * 1.5,
        "2x pitch bend must pitch the output up (more zero crossings); \
         unity={unity} bent={bent}"
    );
}
