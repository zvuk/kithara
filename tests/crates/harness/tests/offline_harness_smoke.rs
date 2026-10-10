#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use kithara::{
    self,
    events::TrackId,
    platform::time::{self, Duration},
    play::{PlayerEvent, Resource, ResourceConfig, ResourceSrc},
    warp::{StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    disk_asset_store,
    offline::{OfflinePlayer, OfflinePlayerOptions, resource_from_reader},
    test_defaults::consts,
};
use kithara_test_fixtures::integration_fixtures::{constant_half, drain_tone};
use kithara_test_utils::{TestTempDir, temp_dir};

const BLOCK_FRAMES: usize = 512;
/// 100 ms of stereo audio at 44.1 kHz: `4_410` frames × 2 channels.
const TARGET_SAMPLES: usize = 8_820;
const MAX_RENDERED_FRAMES: usize = 9_000;
/// Varispeed rate of the Glide scenario.
const GLIDE_RATE: f32 = 1.25;
/// `drain_tone`: four seconds of stereo PCM at 44.1 kHz.
const SOURCE_FRAMES: usize = 176_400;
/// The offline host renders interleaved stereo.
const OUTPUT_CHANNELS: usize = 2;
/// At `GLIDE_RATE` the source plays out in 0.8 of its length. The band keeps
/// out rate 1.0 (a resource off the Warp path needs all of it) and a render
/// that stays silent.
const MIN_SHARE_NUM: usize = 7;
const MAX_SHARE_NUM: usize = 9;
const SHARE_DEN: usize = 10;
const DRAIN_BLOCK_BUDGET: usize = 2_000;

fn make_resource(constant_half: &'static [u8], duration_secs: f64) -> Resource {
    resource_from_reader(kithara::audio::mock::TestPcmReader::with_pcm(
        consts::AUDIO_SPEC,
        duration_secs,
        constant_half,
    ))
}

fn file_resource(
    path: &Path,
    store_dir: &Path,
) -> ResourceConfig<kithara_integration_tests::bufpool_ext::TestPools> {
    let config: ResourceConfig<_> = ResourceConfig::for_src(
        ResourceSrc::parse(path.to_str().expect("utf-8 fixture path"))
            .expect("local media path is a valid resource src"),
    )
    .store(disk_asset_store(store_dir))
    .build();
    config
}

async fn render_target(harness: &OfflinePlayer) -> Vec<f32> {
    let mut rendered: Vec<f32> = Vec::new();
    let mut total_frames: usize = 0;
    while rendered.len() < TARGET_SAMPLES && total_frames < MAX_RENDERED_FRAMES {
        let block = harness.render(BLOCK_FRAMES).await;
        rendered.extend_from_slice(&block);
        total_frames = total_frames.saturating_add(BLOCK_FRAMES);
        let _ = harness.tick_and_drain().await;
    }
    rendered
}

#[kithara::test(tokio)]
async fn offline_harness_smoke(constant_half: &'static [u8]) {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        consts::SAMPLE_RATE,
    )
    .await;
    let deck_source = harness.pcm_deck((make_resource(constant_half, 0.1)).into());
    harness
        .with_queue(move |player| {
            let deck_id = TrackId::allocate();
            player
                .append_with_id(deck_id, deck_source)
                .expect("append PCM deck");
            player
                .select(deck_id, kithara::queue::Transition::None)
                .expect("select the item");
            player.play();
        })
        .await;

    let rendered = render_target(&harness).await;

    assert!(!rendered.is_empty());
    assert!(rendered.iter().any(|sample| sample.abs() > 0.0));
    harness.close().await;
}

/// Audible frames rendered until the item plays to its end. A zero-filled
/// underrun frame is silent, so decode latency does not count.
async fn audible_frames_until_end(harness: &OfflinePlayer) -> usize {
    let mut audible = 0usize;
    for _ in 0..DRAIN_BLOCK_BUDGET {
        let block = harness.render(BLOCK_FRAMES).await;
        audible += block
            .chunks_exact(OUTPUT_CHANNELS)
            .filter(|frame| frame.iter().any(|sample| *sample != 0.0))
            .count();
        let ended = harness
            .tick_and_drain()
            .await
            .iter()
            .any(|event| matches!(event, PlayerEvent::ItemDidPlayToEnd { .. }));
        if ended {
            return audible;
        }
        time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the source never played to its end within {DRAIN_BLOCK_BUDGET} blocks");
}

/// Glide at `GLIDE_RATE` renders through the anti-alias filter on the audio
/// thread; `just test rtsan` runs it under the realtime sanitizer. The source
/// plays out in audibly fewer frames than its length only on the Warp path.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn offline_harness_glide_varispeed(drain_tone: &'static [u8], temp_dir: TestTempDir) {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .warp(WarpConfig::builder().backend(StretchKind::Glide).build())
            .build(),
        consts::SAMPLE_RATE,
    )
    .await;
    let path = temp_dir.write("glide.wav", drain_tone);
    let resource = file_resource(&path, &temp_dir.path().join("store"));
    harness
        .with_queue(move |player| {
            let id = player.append(resource).expect("append local deck");
            player
                .select(id, kithara::queue::Transition::None)
                .expect("select the item");
            player.play();
        })
        .await;
    harness
        .with_queue(move |player| player.set_default_rate(GLIDE_RATE))
        .await
        .expect("a finite rate is accepted");

    let audible = audible_frames_until_end(&harness).await;

    assert!(
        (SOURCE_FRAMES * MIN_SHARE_NUM..=SOURCE_FRAMES * MAX_SHARE_NUM)
            .contains(&(audible * SHARE_DEN)),
        "at rate {GLIDE_RATE} the {SOURCE_FRAMES}-frame source must play out in \
         {MIN_SHARE_NUM}/{SHARE_DEN}..={MAX_SHARE_NUM}/{SHARE_DEN} of its length, got \
         {audible} audible frames"
    );
    harness.close().await;
}
