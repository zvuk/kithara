//! Offline render through the product graph, measured the same way on the
//! browser and on native.
use kithara::{
    assets::{AssetStore, StorageBackend},
    host::{HostConfig, HostSettings, Tap},
    platform::time::Duration,
    play::{ResourceConfig, ResourceSrc, SlotSnapshot},
};
use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
    hls_fixture::create_test_downloader,
    offline::{OfflinePlayer, OfflineWorker},
};
use kithara_test_fixtures::{
    SignalAsset,
    signal::{max_silence_run, peak, rms},
};

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: usize = 2;
const BLOCK_FRAMES: usize = 512;
const WARMUP_BLOCKS: usize = 8;
const MEASURE_BLOCKS: usize = 32;
/// Renders allowed while collecting `MEASURE_BLOCKS` filled blocks.
const DELIVERY_ATTEMPTS: usize = MEASURE_BLOCKS * 16;
const TAP_CAPACITY: usize = 65_536;
const SILENCE_THRESHOLD: f32 = 0.001;
/// The signal route serves every tone at full scale, so a sine has RMS
/// `1/sqrt(2)`; the band leaves room for the limiter and mp3 framing.
const MIN_RMS: f32 = 0.55;
const MAX_RMS: f32 = 0.75;

/// One offline player on the fixture sine, playing with no fade. Everything
/// that waits on the fixture bytes is opened on the harness thread.
async fn playing_worker() -> OfflineWorker {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_SINE440_60S);
    let region = pools();
    let host = HostConfig::offline(region.clone())
        .settings(
            HostSettings::builder()
                .sample_rate(SAMPLE_RATE.try_into().expect("the sample rate is non-zero"))
                .build(),
        )
        .build();
    let worker = OfflineWorker::new(async move || OfflinePlayer::new(host).await).await;
    worker
        .call(async move |player| {
            let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
                ResourceSrc::parse(url.as_str())
                    .expect("the fixture URL parses as a resource source"),
            )
            .downloader(create_test_downloader())
            .worker(player.worker().clone())
            .store(
                AssetStore::builder(region)
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build();

            player.set_fade_duration(0.0).await;
            player.load_and_fadein(config).await;
        })
        .await;
    worker
}

async fn render_blocks(worker: &OfflineWorker, blocks: usize) -> Vec<f32> {
    let mut rendered = Vec::with_capacity(blocks * BLOCK_FRAMES * CHANNELS);
    for _ in 0..blocks {
        let block = worker
            .call(async move |player| player.render(BLOCK_FRAMES).await)
            .await;
        rendered.extend_from_slice(&block);
    }
    rendered
}

fn filled_block(before: &SlotSnapshot, after: &SlotSnapshot) -> bool {
    let (Some(before), Some(after)) = (before.mark, after.mark) else {
        return false;
    };
    before.lane.segment == after.lane.segment
        && after.lane.frame.checked_sub(before.lane.frame) == Some(BLOCK_FRAMES as u64)
        && after.session.frames_since(before.session) == Some(BLOCK_FRAMES as u64)
}

/// Blocks whose consumed lane cursor advanced over every rendered frame.
async fn render_filled_blocks(worker: &OfflineWorker, blocks: usize) -> Vec<f32> {
    let wanted = blocks * BLOCK_FRAMES * CHANNELS;
    let mut filled = Vec::with_capacity(wanted);
    let mut renders = 0usize;
    while filled.len() < wanted {
        assert!(
            renders < DELIVERY_ATTEMPTS,
            "the graph filled {} of {blocks} blocks across {renders} renders",
            filled.len() / (BLOCK_FRAMES * CHANNELS)
        );
        renders += 1;
        let (block, complete) = worker
            .call(async move |player| {
                let before = player.deck_snapshot();
                let block = player.render(BLOCK_FRAMES).await;
                let after = player.deck_snapshot();
                let filled = before
                    .slots
                    .iter()
                    .zip(&after.slots)
                    .any(|(before, after)| filled_block(before, after));
                (block, filled)
            })
            .await;
        if complete {
            filled.extend_from_slice(&block);
        }
    }
    filled
}

/// The offline render carries the fixture's signal, read off complete PCM blocks.
#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(1)
)]
async fn offline_render_carries_fixture_signal() {
    let worker = playing_worker().await;
    let _warmup = render_blocks(&worker, WARMUP_BLOCKS).await;
    let measured = render_filled_blocks(&worker, MEASURE_BLOCKS).await;

    assert_eq!(measured.len(), MEASURE_BLOCKS * BLOCK_FRAMES * CHANNELS);
    let level = rms(&measured);
    assert!(
        (MIN_RMS..=MAX_RMS).contains(&level),
        "full-scale sine renders at RMS {level:.4}, outside {MIN_RMS}..={MAX_RMS}"
    );
    let ceiling = peak(&measured);
    assert!(ceiling <= 1.0, "render clipped at peak {ceiling:.4}");
    let silence = max_silence_run(&measured, 0, measured.len(), SILENCE_THRESHOLD);
    assert!(
        silence < BLOCK_FRAMES * CHANNELS,
        "render went silent for {silence} samples, a whole block or more"
    );
}

#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(1)
)]
async fn offline_mix_tap_mirrors_render() {
    let worker = playing_worker().await;
    let _warmup = render_blocks(&worker, WARMUP_BLOCKS).await;
    let mut tap = worker
        .call(async move |player| player.host().attach_tap(Tap::Master, TAP_CAPACITY).await)
        .await
        .expect("enable the mix tap");

    let rendered = render_blocks(&worker, MEASURE_BLOCKS).await;
    let tapped = tap.drain();

    assert_eq!(tapped, rendered, "the tap carries what the sink received");
    assert_eq!(tap.drops(), 0);
}

#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use kithara::signal::{SegmentId, SessionFrame};
    use kithara_render::{LaneFrame, bridge::SlotMark};

    use super::*;

    #[kithara::test]
    #[case::filled(512, true)]
    #[case::partial(256, false)]
    #[case::sustained_gap(0, false)]
    fn only_fully_consumed_blocks_enter_the_signal_measurement(
        #[case] consumed: u64,
        #[case] expected: bool,
    ) {
        let mark = SlotMark {
            session: SessionFrame::new(1_024),
            lane: LaneFrame {
                segment: SegmentId::FIRST,
                frame: 800,
            },
            position: Duration::ZERO,
        };
        let before = SlotSnapshot {
            mark: Some(mark),
            ..SlotSnapshot::default()
        };
        let mut after = before;
        after.mark = Some(SlotMark {
            session: SessionFrame::new(1_536),
            lane: LaneFrame {
                frame: mark.lane.frame + consumed,
                ..mark.lane
            },
            ..mark
        });
        assert_eq!(filled_block(&before, &after), expected);
        if expected {
            let mut changed_segment = after;
            changed_segment.mark.as_mut().expect("mark").lane.segment = SegmentId::FIRST.next();
            assert!(!filled_block(&before, &changed_segment));
            let mut stale_session = after;
            stale_session.mark.as_mut().expect("mark").session = mark.session;
            assert!(!filled_block(&before, &stale_session));
            assert!(!filled_block(&SlotSnapshot::default(), &after));
        }
    }
}
