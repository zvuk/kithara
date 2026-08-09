use std::num::NonZeroU32;

use kithara::{
    self,
    decode::PcmSpec,
    net::{HttpClient, NetOptions},
    platform::{
        CancelScope,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        thread,
        time::Duration,
    },
    play::{Cmd, MixTapWriter, Resource, SessionDispatcher},
};
use kithara_broadcast::{Broadcast, BroadcastConfig, BroadcastHandle, RingFeed};
use kithara_integration_tests::{
    audio_mock::TestPcmReader, offline::resource_from_reader, signal_pcm::signal::SineWave,
};
use ringbuf::{HeapRb, traits::Split};
use url::Url;

use super::{
    offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions},
    origin::{assert_carries_the_tone, decode_adts_left},
};

/// The engine session's rate. It reaches the broadcast as it is: the packager
/// takes its timescale from whoever feeds it.
const SESSION_RATE: u32 = 44_100;
const TONE_HZ: f64 = 440.0;
const BLOCK_FRAMES: usize = 512;
/// Long enough for the tap to fill several segments and outlast the render.
const TRACK_SECS: f64 = 6.0;
/// Half-second segments at [`SESSION_RATE`]: six of them span the three target
/// durations RFC 8216 §6.2.2 asks a live window for.
const TARGET: Duration = Duration::from_millis(500);
const WINDOW: usize = 6;
/// Frames the AAC-LC encoder needs before the tone is fully formed.
const PRIMING_SKIP_FRAMES: usize = 4_410;
/// Blocks a wait on the packager may render before it is a hang.
const MAX_BLOCKS: usize = 2_000;

fn tone_resource() -> Resource {
    let spec = PcmSpec::new(2, NonZeroU32::new(SESSION_RATE).expect("test rate"));
    resource_from_reader(TestPcmReader::with_signal(
        spec,
        TRACK_SECS,
        SineWave(TONE_HZ),
    ))
}

/// A session with the tone playing, rendered far enough that the session graph
/// and its limiter are live.
fn playing_harness() -> OfflinePlayerHarness {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        SESSION_RATE,
    );
    harness.player().insert(tone_resource(), None, None);
    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");
    harness.render(BLOCK_FRAMES);
    let _ = harness.tick_and_drain();
    harness
}

fn render_blocks(harness: &OfflinePlayerHarness, blocks: usize) -> Vec<f32> {
    let mut rendered = Vec::with_capacity(blocks * BLOCK_FRAMES * 2);
    for _ in 0..blocks {
        rendered.extend_from_slice(&harness.render(BLOCK_FRAMES));
        let _ = harness.tick_and_drain();
    }
    rendered
}

/// Render until `frames` of audible mix have gone by. The decode worker fills
/// the graph at its own pace, so counting blocks would count whatever silence
/// the render outran; counting signal is what the tap is judged on.
fn render_tone(harness: &OfflinePlayerHarness, frames: usize) {
    let mut audible = 0;
    for _ in 0..MAX_BLOCKS {
        let block = harness.render(BLOCK_FRAMES);
        let _ = harness.tick_and_drain();
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

/// Segment URIs the playlist lists after its last break, or `None` when it
/// announces no break at all.
fn segments_after_break(playlist: &str) -> Option<Vec<String>> {
    let mut listed = Vec::new();
    let mut broke = false;
    for line in playlist.lines() {
        if line == "#EXT-X-DISCONTINUITY" {
            broke = true;
            listed.clear();
        } else if !line.starts_with('#') && !line.is_empty() {
            listed.push(line.to_owned());
        }
    }
    broke.then_some(listed)
}

/// The session on air: the tap's ring is the broadcast's intake, and the
/// origin the worker publishes to is reachable over HTTP.
struct OnAir {
    handle: BroadcastHandle,
    scope: CancelScope,
    client: HttpClient,
    base: Url,
    drops: Arc<AtomicU64>,
}

impl OnAir {
    /// Hang a `ring_samples`-deep tap off the session limiter and take the
    /// broadcast on air behind it.
    fn start(harness: &OfflinePlayerHarness, ring_samples: usize) -> Self {
        let (pcm, samples) = HeapRb::<f32>::new(ring_samples).split();
        let drops = Arc::new(AtomicU64::new(0));
        harness
            .session()
            .exec_ok(Cmd::EnableMixTap {
                writer: MixTapWriter::new(pcm, Arc::clone(&drops)),
            })
            .expect("enable the mix tap");

        let config = BroadcastConfig::builder()
            .sample_rate(SESSION_RATE)
            .channels(2)
            .segment_target(TARGET)
            .window(WINDOW)
            .build();
        let scope = CancelScope::new(None);
        let handle = Broadcast::start(
            &config,
            RingFeed::new(samples, Arc::clone(&drops)),
            Some(scope.token()),
        )
        .expect("go on air");
        let base = Url::parse(handle.url()).expect("the handle reports a URL");
        let client = HttpClient::new(NetOptions::default(), scope.token());

        Self {
            handle,
            scope,
            client,
            base,
            drops,
        }
    }

    /// Samples the real-time node found no room for, as it counts them.
    fn tap_drops(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }

    /// Wait until the service has counted every sample the real-time node
    /// lost. Past that point the packager is level with the ring, the break is
    /// behind it, and audio rendered next goes into the discontinuous segment.
    fn wait_for_drain(&self) {
        let lost = self.tap_drops();
        while self.handle.status().dropped_samples < lost {
            thread::paced_backoff(Duration::from_millis(1));
        }
    }

    async fn get(&self, path: &str) -> Vec<u8> {
        let url = self.base.join(path).expect("a servable path");
        self.client
            .get_bytes(url, None)
            .await
            .unwrap_or_else(|error| panic!("the origin refused {path}: {error}"))
            .to_vec()
    }

    async fn media_playlist(&self) -> String {
        String::from_utf8(self.get("v/0/live.m3u8").await).expect("the playlist is text")
    }

    /// Every segment the playlist lists, concatenated in playlist order.
    async fn listed_stream(&self) -> Vec<u8> {
        let playlist = self.media_playlist().await;
        let mut stream = Vec::new();
        for uri in playlist
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
        {
            stream.extend_from_slice(&self.get(&format!("v/0/{uri}")).await);
        }
        stream
    }
}

impl Drop for OnAir {
    fn drop(&mut self) {
        self.scope.cancel();
    }
}

/// What the session renders is the tone the broadcast tests then chase through
/// the encoder and the origin.
#[kithara::test]
fn the_session_renders_the_source_tone() {
    let harness = playing_harness();

    let rendered = render_blocks(&harness, 200);

    assert_carries_the_tone(
        &left_channel(&rendered),
        TONE_HZ,
        SESSION_RATE,
        "the rendered mix",
    );
}

/// The acceptance path: what the engine mixes is what an HTTP client fetches
/// off the origin and decodes.
///
/// `flash(false)`: this body raises a session engine and polls synchronously,
/// which is the pair `kithara-broadcast/CONTEXT.md` names as the one the
/// simulated clock cannot serve.
#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(60)))]
async fn the_engine_mix_reaches_an_http_client_as_the_source_tone() {
    /// Deep enough for every block the render can produce, so a gap here would
    /// be the packager falling behind rather than the ring running out.
    const ROOMY_RING: usize = MAX_BLOCKS * BLOCK_FRAMES * 2;
    /// Two and a half seconds of tone: five half-second segments.
    const TONE_RENDER_FRAMES: usize = 110_250;

    let harness = playing_harness();
    let on_air = OnAir::start(&harness, ROOMY_RING);

    render_tone(&harness, TONE_RENDER_FRAMES);
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
}

/// The other end of the same path: a ring the real-time node overruns becomes
/// an `EXT-X-DISCONTINUITY` in the playlist a client reloads. `flash(false)`
/// for the reason above.
#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(60)))]
async fn a_ring_the_render_outruns_breaks_the_served_playlist() {
    /// A quarter second of stereo: the render fills it long before the
    /// packager can encode its way through.
    const TIGHT_RING: usize = 22_050;
    /// Tone rendered before the break is judged: far past the ring, so the
    /// producer must lose some of it.
    const OVERRUN_FRAMES: usize = 110_250;
    /// Tone rendered after it, sized under the ring so whatever the break cost,
    /// this much reaches the encoder whole.
    const TAIL_FRAMES: usize = 8_820;
    /// Frames the audio after the break must carry to judge its tone: a tenth
    /// of a second spans forty periods of [`TONE_HZ`].
    const TAIL_TONE_FRAMES: usize = 4_410;

    let harness = playing_harness();
    let on_air = OnAir::start(&harness, TIGHT_RING);

    render_tone(&harness, OVERRUN_FRAMES);
    assert!(
        on_air.tap_drops() > 0,
        "the render must outrun a {TIGHT_RING}-sample ring"
    );

    on_air.wait_for_drain();
    render_tone(&harness, TAIL_FRAMES);
    on_air.handle.stop();

    let playlist = on_air.media_playlist().await;
    let after_break = segments_after_break(&playlist).unwrap_or_else(|| {
        panic!("a gap the real-time node counted must reach the client as a break: {playlist}")
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
}
