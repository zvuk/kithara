use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    self,
    broadcast::{Broadcast, BroadcastConfig, BroadcastHandle, BroadcastOutput},
    events::TrackId,
    net::{HttpClient, NetOptions},
    output::{LiveOutput, OutputGroup},
    platform::{CancelScope, time::Duration},
    play::Resource,
    signal::AudioSpec,
    worker::{Worker, WorkerConfig},
};
use kithara_integration_tests::{
    audio_mock::TestPcmReader,
    bufpool_ext::pools,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions, resource_from_reader},
    waits::wait_until,
};
use kithara_test_fixtures::integration_fixtures::broadcast_tone;
use url::Url;

use super::origin::{Playlist, assert_carries_the_tone, decode_adts_left};

const SESSION_RATE: u32 = 44_100;
const TONE_HZ: f64 = 440.0;
const BLOCK_FRAMES: usize = 512;
const TARGET: Duration = Duration::from_millis(500);
const WINDOW: usize = 6;
const PRIMING_SKIP_FRAMES: usize = 4_410;
const MAX_BLOCKS: usize = 2_000;

fn tone_resource(broadcast_tone: Vec<f32>) -> Resource {
    let spec = AudioSpec::new(2, NonZeroU32::new(SESSION_RATE).expect("test rate"));
    resource_from_reader(TestPcmReader::from_samples(spec, broadcast_tone))
}

async fn playing_harness(broadcast_tone: Vec<f32>) -> OfflinePlayerHarness {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        SESSION_RATE,
    )
    .await;
    harness
        .with_player(move |player| {
            player.insert(tone_resource(broadcast_tone), TrackId::allocate(), None);
            player
                .select_item(0, true)
                .expect("select first queue item");
        })
        .await;
    harness.render(BLOCK_FRAMES).await;
    let _ = harness.tick_and_drain().await;
    harness
}

async fn render_blocks(harness: &OfflinePlayerHarness, blocks: usize) -> Vec<f32> {
    let mut rendered = Vec::with_capacity(blocks * BLOCK_FRAMES * 2);
    for _ in 0..blocks {
        rendered.extend_from_slice(&harness.render(BLOCK_FRAMES).await);
        let _ = harness.tick_and_drain().await;
    }
    rendered
}

async fn render_tone(harness: &OfflinePlayerHarness, frames: usize) {
    let mut audible = 0;
    for _ in 0..MAX_BLOCKS {
        let block = harness.render(BLOCK_FRAMES).await;
        let _ = harness.tick_and_drain().await;
        audible += block.iter().step_by(2).filter(|s| s.abs() > 0.0).count();
        if audible >= frames {
            return;
        }
    }
    panic!("the session rendered {audible} of {frames} audible frames over {MAX_BLOCKS} blocks");
}

fn left_channel(interleaved: &[f32]) -> Vec<f32> {
    interleaved.iter().step_by(2).copied().collect()
}

struct OnAir {
    handle: BroadcastHandle,
    _worker: Worker,
    scope: CancelScope,
    client: HttpClient,
    base: Url,
}

struct GapOutput {
    gap_after_writes: Option<usize>,
    output: BroadcastOutput,
    writes: usize,
}

impl LiveOutput for GapOutput {
    fn reconfigure(&mut self, spec: AudioSpec) {
        self.output.reconfigure(spec);
    }

    fn write_stereo(&mut self, frames: usize, left: &[f32], right: &[f32]) {
        if self.gap_after_writes == Some(self.writes) {
            self.output.write_stereo(frames, &[], &[]);
        }
        self.output.write_stereo(frames, left, right);
        self.writes = self.writes.saturating_add(1);
    }
}

impl OnAir {
    /// Non-progress watchdog: the waits resolve as soon as the packager reports.
    const DRAIN_DEADLINE: Duration = Duration::from_secs(20);
    /// Polls the segment count must repeat before the packager counts as idle.
    const SETTLED_POLLS: usize = 3;

    async fn get(&self, path: &str) -> Vec<u8> {
        let url = self.base.join(path).expect("a servable path");
        self.client
            .get_bytes(url, None)
            .await
            .unwrap_or_else(|error| panic!("the origin refused {path}: {error}"))
            .to_vec()
    }

    async fn listed_stream(&self) -> Vec<u8> {
        let playlist = Playlist::parse(self.media_playlist().await);
        let mut stream = Vec::new();
        for entry in &playlist.entries {
            stream.extend_from_slice(&self.get(&format!("v/0/{}", entry.uri)).await);
        }
        stream
    }

    async fn media_playlist(&self) -> String {
        String::from_utf8(self.get("v/0/live.m3u8").await).expect("the playlist is text")
    }

    async fn start(
        harness: &OfflinePlayerHarness,
        ring_samples: usize,
        gap_after_writes: Option<usize>,
    ) -> Self {
        let buffer_frames = ring_samples / 2;
        let scope = CancelScope::new(None);
        let worker = Worker::new(WorkerConfig::new());
        let pools = pools();
        let config = BroadcastConfig::builder(worker.clone(), pools.clone())
            .cancel(scope.token())
            .sample_rate(SESSION_RATE)
            .channels(2)
            .segment_target(TARGET)
            .window(WINDOW)
            .buffer_frames(
                NonZeroUsize::new(buffer_frames).expect("test broadcast buffer is non-zero"),
            )
            .build();
        let (output, handle) = Broadcast::start(config).expect("go on air");
        let mut outputs = OutputGroup::new();
        outputs.push(GapOutput {
            gap_after_writes,
            output,
            writes: 0,
        });
        harness
            .host()
            .enable_outputs(outputs)
            .await
            .expect("enable the master output group");
        let base = Url::parse(handle.url()).expect("the handle reports a URL");
        let client = HttpClient::new(NetOptions::default(), pools, scope.token());

        Self {
            handle,
            _worker: worker,
            scope,
            client,
            base,
        }
    }

    /// Wait until the packager has eaten the ring empty: with the render
    /// stopped, the segment count settles once there is nothing left to
    /// package. Audio pushed after that starts against an empty ring, so a
    /// render that fits the ring cannot break the stream a second time.
    async fn wait_until_drained(&self) {
        wait_until(Self::DRAIN_DEADLINE, "the packager reads the drops", || {
            self.handle.status().dropped_samples > 0
        })
        .await
        .expect("the packager accounts for every dropped sample");

        // `u64::MAX` is no segment count, so the first observation resets the
        // counter and the settle window stays three later polls.
        let mut settled = 0;
        let mut last = u64::MAX;
        wait_until(Self::DRAIN_DEADLINE, "the segment count settles", || {
            let segments = self.handle.status().segments;
            settled = if segments == last { settled + 1 } else { 0 };
            last = segments;
            settled >= Self::SETTLED_POLLS
        })
        .await
        .expect("the packager stops producing segments once the ring is empty");
    }
}

impl Drop for OnAir {
    fn drop(&mut self) {
        self.scope.cancel();
    }
}

#[kithara::test(tokio)]
async fn the_session_renders_the_source_tone(broadcast_tone: Vec<f32>) {
    let harness = playing_harness(broadcast_tone).await;

    let rendered = render_blocks(&harness, 200).await;

    assert_carries_the_tone(
        &left_channel(&rendered),
        TONE_HZ,
        SESSION_RATE,
        "the rendered mix",
    );
    harness.close().await;
}

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(60)))]
async fn the_engine_mix_reaches_an_http_client_as_the_source_tone(broadcast_tone: Vec<f32>) {
    const ROOMY_RING: usize = MAX_BLOCKS * BLOCK_FRAMES * 2;
    const TONE_RENDER_FRAMES: usize = 110_250;

    let harness = playing_harness(broadcast_tone).await;
    let on_air = OnAir::start(&harness, ROOMY_RING, None).await;

    render_tone(&harness, TONE_RENDER_FRAMES).await;
    on_air.handle.stop();

    let decoded = decode_adts_left(on_air.listed_stream().await);

    assert_eq!(
        on_air.handle.status().dropped_samples,
        0,
        "a ring sized for the whole render leaves the packager no reason to drop"
    );
    assert!(
        decoded.len() >= TONE_RENDER_FRAMES / 2,
        "the fetched segments must carry the tone that went on air, got {} of \
         {TONE_RENDER_FRAMES} frames",
        decoded.len()
    );
    assert_carries_the_tone(
        &decoded[PRIMING_SKIP_FRAMES..],
        TONE_HZ,
        SESSION_RATE,
        "the fetched stream",
    );
    drop(on_air);
    harness.close().await;
}

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(60)))]
async fn an_intake_gap_breaks_the_served_playlist(broadcast_tone: Vec<f32>) {
    const ROOMY_RING: usize = MAX_BLOCKS * BLOCK_FRAMES * 2;
    const GAP_AFTER_WRITES: usize = 100;
    const OVERRUN_FRAMES: usize = 110_250;
    const TAIL_FRAMES: usize = 8_820;
    const TAIL_TONE_FRAMES: usize = 4_410;

    let harness = playing_harness(broadcast_tone).await;
    let on_air = OnAir::start(&harness, ROOMY_RING, Some(GAP_AFTER_WRITES)).await;

    render_tone(&harness, OVERRUN_FRAMES).await;
    on_air.wait_until_drained().await;
    render_tone(&harness, TAIL_FRAMES).await;
    on_air.handle.stop();

    let playlist = Playlist::parse(on_air.media_playlist().await);
    let after_break = playlist.uris_after_last_discontinuity().unwrap_or_else(|| {
        panic!(
            "a gap the real-time node counted must reach the client as a break: {}",
            playlist.text
        )
    });
    assert!(
        on_air.handle.status().dropped_samples > 0,
        "the service counts what the producer lost"
    );

    let mut stream = Vec::new();
    for uri in &after_break {
        stream.extend_from_slice(&on_air.get(&format!("v/0/{uri}")).await);
    }
    let decoded = decode_adts_left(stream);

    assert!(
        decoded.len() >= TAIL_TONE_FRAMES,
        "the segments after the break must carry {TAIL_TONE_FRAMES} frames to judge, got {}",
        decoded.len()
    );
    assert_carries_the_tone(
        &decoded[decoded.len() - TAIL_TONE_FRAMES..],
        TONE_HZ,
        SESSION_RATE,
        "the stream after the break",
    );
    drop(on_air);
    harness.close().await;
}
