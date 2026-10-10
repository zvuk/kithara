#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    audio::mock::TestPcmReader,
    effects::LimiterConfig,
    host::{
        HostConfig, HostSettings, HostSettingsChange, HostSettingsControl, MetronomeConfig,
        MetronomeConfigChange, MetronomeConfigControl, Tap,
    },
    play::{PlayError, Tempo},
    signal::{AudioSpec, SessionFrame},
    warp::{Beat, BeatGridQuery, BeatGridSnapshot, BeatOrdinal, MapPoint, MapPosition},
};
use kithara_command::When;
use kithara_config::Configure;
use kithara_integration_tests::{
    audio_artifact::{AudioArtifactTap, artifact_label},
    bufpool_ext::{TestPools, pools},
    offline::{
        OfflineHostHarness, OfflinePlayer, OfflinePlayerOptions, TapProbe, resource_from_reader,
    },
};
use kithara_test_fixtures::{
    analysis_beat_fixtures::sine_440_long, integration_fixtures::constant_half,
};
use num_traits::AsPrimitive;

use super::mix_tap::{ROOMY_CAPACITY, play_resource, playing_harness, render_blocks};

mod consts {
    pub(super) const SAMPLE_RATE: u32 = 44_100;
    /// Twice [`SAMPLE_RATE`]: a route restart between the two rates halves
    /// or doubles the frame grid the Host beats round to.
    pub(super) const DOUBLE_RATE: u32 = 88_200;
    pub(super) const BLOCK_FRAMES: u32 = 512;
    pub(super) const CHANNELS: u16 = 2;
    /// A tempo whose beat is not a whole number of frames, so every click
    /// lands on a frame the anchor rounds to.
    pub(super) const BPM: f64 = 124.0;
    pub(super) const BLOCKS: u64 = 700;
    pub(super) const BEATS_PER_BAR: i64 = 4;
    /// Peak of a downbeat click at the default metronome level: the default
    /// limiter ceiling.
    pub(super) const DOWNBEAT_PEAK: f32 = 0.98;
    /// Peak of a beat click: five eighths of a downbeat.
    pub(super) const BEAT_PEAK: f32 = 0.6125;
    pub(super) const PEAK_TOLERANCE: f32 = 1e-6;
    /// How far under its level a click's sampled peak may fall: the samples
    /// nearest the crests of the tone around the envelope's peak miss the
    /// crest by less than this at every rate from 44.1 kHz.
    pub(super) const PEAK_SHORTFALL: f32 = 0.01;
    /// A click rises from silence: its beat frame carries the silent foot of
    /// the rise, so it first sounds one frame later.
    pub(super) const SILENT_FOOT: u64 = 1;
    /// Frames of one click at [`SAMPLE_RATE`]: its 2 ms rise and its default
    /// 35 ms fall.
    pub(super) const CLICK_FRAMES: u64 = 1632;
    /// Frames of the 2 ms rise of a click and its duck at [`SAMPLE_RATE`].
    pub(super) const ATTACK_FRAMES: u64 = 88;
    /// Frames the default duck holds the deck silent after a click at
    /// [`SAMPLE_RATE`]: 20 ms.
    pub(super) const HOLD_FRAMES: u64 = 882;
    /// Frames the default duck takes to return the deck at [`SAMPLE_RATE`]:
    /// 80 ms.
    pub(super) const RELEASE_FRAMES: u64 = 3528;
    /// How far the gain the duck leaves the deck may stray from its curve.
    pub(super) const GAIN_TOLERANCE: f32 = 1e-6;
    /// Blocks rendered after a pause so the deck's fade-out has settled.
    pub(super) const SETTLE_BLOCKS: usize = 4;
    /// Blocks rendered with every deck paused: more than two beats at [`BPM`].
    pub(super) const PAUSED_BLOCKS: usize = 100;
    /// Blocks rendered over a loud mix: the first block, one beat at [`BPM`]
    /// and a click's whole duck, so the duck sounds wherever the beat falls.
    pub(super) const LOUD_BLOCKS: usize = 52;
    pub(super) const LOUD_CEILING: f32 = 0.25;
    /// Peak of a downbeat click as a share of [`LOUD_CEILING`].
    pub(super) const LOUD_LEVEL: f32 = 0.8;
    /// The shallowest duck that keeps [`LOUD_LEVEL`] under the ceiling: the
    /// ducked mix plus the click meets it exactly.
    pub(super) const LOUD_DUCK: f32 = 0.8;
    /// The least share of [`LOUD_CEILING`] a steady mix at twice the ceiling
    /// settles at. The limiter's inter-sample detector passes a constant with
    /// a gain just over one, so it holds that mix 0.084 % under the ceiling;
    /// bounded at twice that.
    pub(super) const LOUD_SETTLED: f32 = 0.9983;
    /// The Host tempo a ride starts from.
    pub(super) const RIDE_FROM_BPM: u32 = 120;
    /// The Host tempo a ride ends at.
    pub(super) const RIDE_TO_BPM: u32 = 145;
    /// Beats a ride holds at each end: two bars.
    pub(super) const HOLD_BEATS: usize = 8;
    /// Blocks a tempo change may take to reach the published grid.
    pub(super) const GRID_WAIT_BLOCKS: usize = 4;
    /// Blocks rendered after a ride's last beat so its click ends in the take.
    pub(super) const CLICK_TAIL_BLOCKS: usize = 2;
    /// The louder of two decks that differ in nothing else.
    pub(super) const DECK_LOUD: f32 = 0.5;
    /// The softer of two decks that differ in nothing else.
    pub(super) const DECK_SOFT: f32 = 0.25;
    /// The default metronome level: a downbeat click at the limiter ceiling.
    pub(super) const FULL_LEVEL: f32 = 1.0;
    pub(super) const HALF_LEVEL: f32 = 0.5;
    /// Over one: a level the Host refuses.
    pub(super) const OVER_LEVEL: f32 = 1.5;
    /// Blocks the first click may take to sound once the tempo commits.
    pub(super) const FIRST_CLICK_BLOCKS: u64 = 4;
    /// A quarter of the attack: rendered in these steps, a level change
    /// lands before the first click peaks.
    pub(super) const LEVEL_STEP_FRAMES: u64 = ATTACK_FRAMES / 4;
    /// The Host beat whose click ends a render of a level change: a bar on.
    pub(super) const LEVEL_BEATS: i64 = 4;
    /// The tempo a take starts at: the one a Host never given a tempo counts.
    pub(super) const TAKE_FROM_BPM: f64 = 120.0;
    /// The tempo a take moves its Host to between two beats.
    pub(super) const TAKE_TO_BPM: f64 = 128.0;
    /// The beat of a take's first click: the downbeat of its second bar.
    pub(super) const TAKE_ON_BEAT: i64 = 4;
    /// The beat of a take's first click at half the level: the one after
    /// the downbeat of its third bar.
    pub(super) const TAKE_LEVEL_BEAT: i64 = 9;
    /// The beat a take's tempo moves halfway after.
    pub(super) const TAKE_TEMPO_BEAT: i64 = 10;
    /// Beats a take renders at its new tempo: two bars.
    pub(super) const TAKE_MOVED_BEATS: u64 = 8;
    /// Frames of one beat at the 120 BPM a take starts at.
    pub(super) const TAKE_BEAT_FRAMES: u64 = 22_050;
    /// Frames the Host smooths a tempo step over at [`SAMPLE_RATE`]: 5 ms,
    /// rounded up.
    pub(super) const TEMPO_SMOOTH_FRAMES: f64 = 221.0;
    /// How far a click may fall from a beat spacing worked out in fractional
    /// frames: the grid rounds every beat to a whole frame.
    pub(super) const BEAT_ROUNDING_FRAMES: f64 = 1.0;
    /// Frames after Host beat 1 a timed change lands on: inside the block of
    /// that beat at [`BLOCK_FRAMES`], with the beat at 120 BPM.
    pub(super) const AFTER_BEAT_FRAMES: u64 = 50;
    /// Frames between two tempo changes due in one block: longer than
    /// [`TEMPO_SMOOTH_FRAMES`], so the first settles before the second.
    pub(super) const BETWEEN_CHANGES_FRAMES: u64 = 300;
    /// The tempo the first of two changes in one block moves to.
    pub(super) const FIRST_CHANGE_BPM: f64 = 60.0;
    /// The tempo the second of two changes in one block moves to.
    pub(super) const SECOND_CHANGE_BPM: f64 = 180.0;
}

/// One click the output tap sounded: the first frame it sounds on, how many
/// frames it sounds for and its peak.
#[derive(Debug)]
pub(super) struct Click {
    pub(super) frame: u64,
    pub(super) frames: u64,
    pub(super) peak: f32,
}

/// Every click in `pcm`. A click opens on a sounding frame after silence and
/// ends where two frames in a row are silent: its own waveform may cross
/// zero on a single sample, silence lasts longer.
pub(super) fn clicks(pcm: &[f32]) -> Vec<Click> {
    let channels = usize::from(consts::CHANNELS);
    let mut found: Vec<Click> = Vec::new();
    let mut last_sounding: Option<u64> = None;
    for (frame, samples) in pcm.chunks_exact(channels).enumerate() {
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        if peak == 0.0 {
            continue;
        }
        let frame = frame as u64;
        match found.last_mut() {
            Some(click) if last_sounding.is_some_and(|last| frame - last <= 2) => {
                click.frames = frame - click.frame + 1;
                click.peak = click.peak.max(peak);
            }
            _ => found.push(Click {
                frame,
                frames: 1,
                peak,
            }),
        }
        last_sounding = Some(frame);
    }
    found
}

/// Every click in `heard` peaks at the peak of the Host beat it rises from,
/// a downbeat's or a beat's, at metronome `level`.
pub(super) fn assert_click_levels(heard: &[Click], beats: &[(u64, bool)], level: f32) {
    for (click, (_, downbeat)) in heard.iter().zip(beats) {
        let expected = level
            * if *downbeat {
                consts::DOWNBEAT_PEAK
            } else {
                consts::BEAT_PEAK
            };
        assert!(
            click.peak <= expected + consts::PEAK_TOLERANCE
                && click.peak >= expected * (1.0 - consts::PEAK_SHORTFALL),
            "a {} click peaks at {expected}: {click:?}",
            if *downbeat { "downbeat" } else { "beat" }
        );
    }
}

/// The session frame of Host beat `ordinal`.
pub(super) fn beat_frame(grid: &BeatGridSnapshot, ordinal: i64) -> u64 {
    let beat = Beat::try_from(BeatOrdinal::new(ordinal)).expect("whole Host beat");
    let BeatGridQuery::Resolved(position) = grid.position_at(MapPoint::new(grid.stamp(), beat))
    else {
        panic!("the Host grid places beat {ordinal}");
    };
    let MapPosition::Session(frame) = *position.value().value() else {
        panic!("the Host grid is on the session axis");
    };
    u64::try_from(i64::from(frame)).expect("Host beat after the session start")
}

/// The session frame of every whole Host beat inside `frames`, with whether
/// it opens a bar. Beat 0 is the tempo commit frame; there are no earlier beats.
fn host_beats(grid: &BeatGridSnapshot, frames: std::ops::Range<u64>) -> Vec<(u64, bool)> {
    (0_i64..)
        .map(|ordinal| {
            (
                beat_frame(grid, ordinal),
                ordinal % consts::BEATS_PER_BAR == 0,
            )
        })
        .take_while(|(frame, _)| *frame < frames.end)
        .filter(|(frame, _)| frames.contains(frame))
        .collect()
}

/// The first Host beat of `grid` at or after `frame`, with whether it opens a
/// bar.
fn beat_from(grid: &BeatGridSnapshot, frame: u64) -> (u64, bool) {
    (0_i64..)
        .map(|ordinal| {
            (
                beat_frame(grid, ordinal),
                ordinal % consts::BEATS_PER_BAR == 0,
            )
        })
        .find(|(beat, _)| *beat >= frame)
        .expect("the Host grid runs past every frame")
}

/// The Host tempo of every beat of a ride: two bars at
/// [`consts::RIDE_FROM_BPM`], one BPM more on each beat up to
/// [`consts::RIDE_TO_BPM`], two bars there.
fn ride() -> Vec<u32> {
    let hold = |bpm| std::iter::repeat_n(bpm, consts::HOLD_BEATS);
    hold(consts::RIDE_FROM_BPM)
        .chain(consts::RIDE_FROM_BPM + 1..=consts::RIDE_TO_BPM)
        .chain(hold(consts::RIDE_TO_BPM))
        .collect()
}

fn peak(pcm: &[f32]) -> f32 {
    pcm.iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

pub(super) fn capacity(frames: u64) -> usize {
    usize::try_from(frames).expect("tap capacity") * usize::from(consts::CHANNELS)
}

fn tempo() -> Tempo {
    Tempo::new(consts::BPM).expect("fixture tempo")
}

/// An offline Host at `sample_rate` with no deck, its metronome on, its
/// transport running at [`consts::BPM`], and an output tap holding `frames`
/// frames. The tempo is set before the first block, so the transport starts
/// at it with beat 0 on session frame 0.
async fn metronome_host(
    sample_rate: u32,
    frames: u64,
) -> (OfflineHostHarness<TestPools>, TapProbe) {
    let sample_rate = NonZeroU32::new(sample_rate).expect("test sample rate");
    let config = HostConfig::offline(pools())
        .settings(HostSettings::builder().sample_rate(sample_rate).build())
        .max_block_frames(NonZeroU32::new(consts::BLOCK_FRAMES).expect("test block size"))
        .build();
    let host = OfflineHostHarness::new(config)
        .await
        .expect("offline Host without a deck");
    let tempo = tempo();
    host.with(move |host| host.set_tempo(tempo))
        .await
        .expect("Host tempo");
    let tap = host
        .attach_tap(Tap::Output, capacity(frames))
        .await
        .expect("output tap");
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    (host, tap)
}

/// An offline Host at [`consts::SAMPLE_RATE`] with no deck, counting the
/// tempo a Host never given one counts, its metronome off, and an output tap
/// holding `frames` frames from the first block.
async fn counting_host(frames: u64) -> (OfflineHostHarness<TestPools>, TapProbe) {
    let rate = NonZeroU32::new(consts::SAMPLE_RATE).expect("test sample rate");
    let config = HostConfig::offline(pools())
        .settings(HostSettings::builder().sample_rate(rate).build())
        .max_block_frames(NonZeroU32::new(consts::BLOCK_FRAMES).expect("test block size"))
        .build();
    let host = OfflineHostHarness::new(config)
        .await
        .expect("offline Host without a deck");
    let tap = host
        .attach_tap(Tap::Output, capacity(frames))
        .await
        .expect("output tap");
    (host, tap)
}

#[kithara::test(tokio)]
async fn the_engine_metronome_clicks_on_every_host_beat_with_no_deck_playing() {
    let frames = consts::BLOCKS * u64::from(consts::BLOCK_FRAMES);
    let (host, mut tap) = metronome_host(consts::SAMPLE_RATE, frames).await;

    let rendered = host.render_forward(frames).await;
    let pcm = tap.drain();
    let grid = host.session_grid().await;
    host.close().await;

    if let Some(mut artifact) =
        AudioArtifactTap::from_env(&artifact_label(), consts::SAMPLE_RATE, consts::CHANNELS)
            .expect("listening artifact")
    {
        artifact.push(&pcm);
    }
    assert_eq!(rendered, frames, "the Host renders every requested frame");
    assert_eq!(tap.drops(), 0, "the tap keeps every frame");
    assert_eq!(
        pcm.len(),
        capacity(frames),
        "the tap sees every rendered frame"
    );
    let beats = host_beats(&grid, 0..frames);
    let heard = clicks(&pcm);
    assert!(
        beats.len() >= 3 * usize::try_from(consts::BEATS_PER_BAR).expect("bar length"),
        "the render spans several bars: {beats:?}"
    );
    assert_eq!(
        heard.iter().map(|click| click.frame).collect::<Vec<_>>(),
        beats
            .iter()
            .map(|(frame, _)| frame + consts::SILENT_FOOT)
            .collect::<Vec<_>>(),
        "one click rises from the frame of every Host beat, and nowhere else"
    );
    assert_click_levels(&heard, &beats, consts::FULL_LEVEL);
}

#[kithara::test(tokio)]
async fn a_host_never_given_a_tempo_counts_and_clicks_at_120_bpm() {
    let frames = consts::BLOCKS * u64::from(consts::BLOCK_FRAMES);
    let config = HostConfig::offline(pools())
        .settings(
            HostSettings::builder()
                .sample_rate(NonZeroU32::new(consts::SAMPLE_RATE).expect("test sample rate"))
                .build(),
        )
        .max_block_frames(NonZeroU32::new(consts::BLOCK_FRAMES).expect("test block size"))
        .build();
    let host = OfflineHostHarness::new(config)
        .await
        .expect("offline Host without a deck");
    let mut tap = host
        .attach_tap(Tap::Output, capacity(frames))
        .await
        .expect("output tap");
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");

    host.render_forward(frames).await;
    let pcm = tap.drain();
    let tempo = host.with(|host| host.tempo()).await;
    host.close().await;

    assert_eq!(tempo, Tempo::new(120.0).expect("the default tempo"));
    let frames_per_beat = u64::from(consts::SAMPLE_RATE) / 2;
    assert_eq!(
        clicks(&pcm)
            .iter()
            .map(|click| click.frame)
            .collect::<Vec<_>>(),
        (0..)
            .map(|beat| beat * frames_per_beat)
            .take_while(|frame| *frame < frames)
            .map(|frame| frame + consts::SILENT_FOOT)
            .collect::<Vec<_>>(),
        "a click rises on every beat at 120 BPM from the first frame"
    );
}

#[kithara::test(tokio)]
async fn a_click_a_route_restart_interrupts_ends_when_it_would_have_at_the_new_rate() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let (host, mut tap) = metronome_host(consts::SAMPLE_RATE, consts::BLOCKS * block).await;
    host.render_forward(block).await;
    let beat_one = beat_frame(&host.session_grid().await, 1);
    host.render_forward(beat_one - host.position()).await;
    let whole = clicks(&tap.drain());
    let [first] = whole.as_slice() else {
        panic!("one click sounds before beat 1: {whole:?}");
    };

    let span = first.frames + consts::SILENT_FOOT;
    let head_frames = span / 2;
    host.render_forward(head_frames).await;
    let head = clicks(&tap.drain());
    host.set_sample_rate(NonZeroU32::new(consts::DOUBLE_RATE).expect("restart rate"))
        .await
        .expect("restart the route at the new rate");
    host.render_forward(4 * first.frames).await;
    let tail = clicks(&tap.drain());
    host.close().await;

    assert_eq!(
        head.iter().map(|click| click.frames).collect::<Vec<_>>(),
        [head_frames - consts::SILENT_FOOT],
        "beat 1's click is sounding when the route restarts"
    );
    let [tail] = tail.as_slice() else {
        panic!("only the interrupted click sounds after the restart: {tail:?}");
    };
    let scale = f64::from(consts::DOUBLE_RATE) / f64::from(consts::SAMPLE_RATE);
    let remaining: f64 = (span - head_frames).as_();
    let expected = remaining * scale;
    let tail_frames: f64 = tail.frames.as_();
    assert_eq!(tail.frame, 0, "the click carries on across the restart");
    assert!(
        (tail_frames - expected).abs() <= scale,
        "the click ends when it would have: {} frames at the new rate, expected {expected:.0}",
        tail.frames
    );
}

/// Renders `host` up to `past` frames after Host beat 1, restarts the route at
/// `rate`, and returns the clicks the output tap sounds in the blocks after
/// the restart.
async fn clicks_after_a_restart_near_beat_one(
    host: OfflineHostHarness<TestPools>,
    mut tap: TapProbe,
    past: u64,
    rate: u32,
) -> Vec<Click> {
    let block = u64::from(consts::BLOCK_FRAMES);
    host.render_forward(block).await;
    let beat_one = beat_frame(&host.session_grid().await, 1);
    host.render_forward(beat_one + past - host.position()).await;
    tap.drain();
    host.set_sample_rate(NonZeroU32::new(rate).expect("restart rate"))
        .await
        .expect("restart the route at the new rate");
    host.render_forward(4 * block).await;
    let tail = clicks(&tap.drain());
    host.close().await;
    tail
}

#[kithara::test(tokio)]
async fn a_beat_a_restart_to_a_lower_rate_lands_on_clicks_once() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let (host, tap) = metronome_host(consts::DOUBLE_RATE, consts::BLOCKS * block).await;
    // WHY: At [`consts::BPM`] beat 1 lies 0.42 frames after the frame it
    // rounds to at the double rate. A restart one frame later puts it 0.29
    // frames before the restart frame at the lower rate: onto which it rounds
    // again.
    let tail =
        clicks_after_a_restart_near_beat_one(host, tap, consts::SILENT_FOOT, consts::SAMPLE_RATE)
            .await;

    let [tail] = tail.as_slice() else {
        panic!("only beat 1's click sounds after the restart: {tail:?}");
    };
    assert_eq!(
        tail.frame, 0,
        "beat 1's click carries on across the restart instead of starting again"
    );
}

#[kithara::test(tokio)]
async fn a_beat_a_restart_to_a_higher_rate_lands_on_still_clicks() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let (host, tap) = metronome_host(consts::SAMPLE_RATE, consts::BLOCKS * block).await;
    // WHY: At [`consts::BPM`] beat 1 lies 0.29 frames before the frame it
    // rounds to, the restart frame; at the double rate that is 0.58 frames,
    // which rounds onto the frame before the restart.
    let tail = clicks_after_a_restart_near_beat_one(host, tap, 0, consts::DOUBLE_RATE).await;

    let [tail] = tail.as_slice() else {
        panic!("beat 1 clicks once after the restart: {tail:?}");
    };
    assert_eq!(
        tail.frame,
        consts::SILENT_FOOT,
        "beat 1's click rises from the first frame after the restart"
    );
}

#[kithara::test(tokio)]
async fn a_metronome_switched_back_on_clicks_from_the_next_beat() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let frames = consts::BLOCKS * block;
    let (host, mut tap) = metronome_host(consts::SAMPLE_RATE, frames).await;
    host.render_forward(block).await;
    let beat_three = beat_frame(&host.session_grid().await, 3);
    host.with(|host| host.metronome().set_enabled(false))
        .await
        .expect("metronome off");
    host.render_forward(beat_three + block - host.position())
        .await;
    tap.drain();
    let start = host.position();
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome back on");
    host.render_forward(frames - start).await;
    let heard: Vec<u64> = clicks(&tap.drain())
        .iter()
        .map(|click| click.frame + start)
        .collect();
    let grid = host.session_grid().await;
    host.close().await;

    let beats: Vec<u64> = host_beats(&grid, start..frames)
        .into_iter()
        .map(|(frame, _)| frame + consts::SILENT_FOOT)
        .collect();
    assert!(!beats.is_empty(), "the render spans Host beats");
    assert_eq!(
        heard, beats,
        "the beats the metronome was off for stay silent; it clicks from the next beat on"
    );
}

#[kithara::test(tokio)]
async fn the_metronome_is_off_by_default_and_passes_the_mix_bit_exactly(
    constant_half: &'static [u8],
) {
    let harness = playing_harness(constant_half).await;
    let mut master = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap");
    let mut output = harness
        .host()
        .attach_tap(Tap::Output, ROOMY_CAPACITY)
        .await
        .expect("output tap");
    let tempo = tempo();
    harness
        .host()
        .with(move |host| host.set_tempo(tempo))
        .await
        .expect("Host tempo");
    let rendered = render_blocks(&harness, 20).await;
    harness.close().await;

    let master = master.drain();
    assert_eq!(
        master, rendered,
        "the master tap carries graph_out while the metronome is off"
    );
    assert_eq!(
        output.drain(),
        master,
        "an off metronome passes the limited mix bit-exactly"
    );
}

#[kithara::test(tokio)]
async fn the_master_tap_carries_no_click() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let frames = 100 * block;
    let (host, mut output) = metronome_host(consts::SAMPLE_RATE, frames).await;
    let mut master = host
        .attach_tap(Tap::Master, capacity(frames))
        .await
        .expect("master tap");
    host.render_forward(frames - block).await;
    host.close().await;

    assert!(
        master.drain().iter().all(|sample| *sample == 0.0),
        "the master tap is the limited mix alone: silence with no deck"
    );
    assert!(!clicks(&output.drain()).is_empty(), "the output tap clicks");
}

#[kithara::test(tokio)]
async fn the_metronome_clicks_on_host_beats_while_every_deck_is_paused(
    constant_half: &'static [u8],
) {
    let harness = playing_harness(constant_half).await;
    harness
        .with_queue(kithara::queue::QueueControl::pause)
        .await;
    render_blocks(&harness, consts::SETTLE_BLOCKS).await;

    let mut master = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY * 2)
        .await
        .expect("master tap");
    let mut output = harness
        .host()
        .attach_tap(Tap::Output, ROOMY_CAPACITY * 2)
        .await
        .expect("output tap");
    harness
        .host()
        .with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    let tempo = tempo();
    harness
        .host()
        .with(move |host| host.set_tempo(tempo))
        .await
        .expect("Host tempo");

    let start = harness.host().position();
    let rendered = render_blocks(&harness, consts::PAUSED_BLOCKS).await;
    let end = harness.host().position();
    let grid = harness.host().session_grid().await;
    harness.close().await;

    let output = output.drain();
    assert!(
        master.drain().iter().all(|sample| *sample == 0.0),
        "every deck is paused: the master tap is silent"
    );
    assert_eq!(
        output, rendered,
        "the output tap carries what graph_out plays"
    );
    let heard: Vec<u64> = clicks(&output)
        .iter()
        .map(|click| click.frame + start)
        .collect();
    let beats: Vec<u64> = host_beats(&grid, start..end)
        .into_iter()
        .map(|(frame, _)| frame + consts::SILENT_FOOT)
        .collect();
    assert!(!beats.is_empty(), "the render spans Host beats");
    assert_eq!(
        heard, beats,
        "the metronome follows the Host grid with no deck playing"
    );
}

/// A tempo ride rendered over a deck: both taps, the session frame they
/// start on and every Host beat of the ride with whether it opens a bar.
struct Ride {
    output: Vec<f32>,
    master: Vec<f32>,
    start: u64,
    beats: Vec<(u64, bool)>,
}

/// Rides the Host tempo from [`consts::RIDE_FROM_BPM`] to
/// [`consts::RIDE_TO_BPM`] with the metronome on over a deck playing `tone`.
async fn ride_over(tone: Vec<f32>) -> Ride {
    let frames = u64::try_from(tone.len()).expect("tone length");
    let rate = NonZeroU32::new(consts::SAMPLE_RATE).expect("test sample rate");
    let harness = play_resource(
        OfflinePlayer::with_sample_rate(
            OfflinePlayerOptions::builder().build(),
            consts::SAMPLE_RATE,
        )
        .await,
        move || {
            resource_from_reader(TestPcmReader::with_samples(
                AudioSpec::new(consts::CHANNELS, rate),
                tone,
            ))
        },
    )
    .await;
    let host = harness.host();
    let mut master = host
        .attach_tap(Tap::Master, capacity(frames))
        .await
        .expect("master tap");
    let mut output = host
        .attach_tap(Tap::Output, capacity(frames))
        .await
        .expect("output tap");
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    let start = host.position();

    // WHY: A tempo change commits one block after it is set, onto a grid
    // retargeted from that frame. Setting it on the block boundary just after
    // a click sounds keeps every beat out of that window, so each beat is
    // placed by the grid of the step it belongs to.
    let mut beats = Vec::new();
    for step in ride() {
        let from = host.position();
        let tempo = Tempo::new(f64::from(step)).expect("ride tempo");
        if host.with(|host| host.tempo()).await != tempo {
            let revision = host.session_grid().await.revision();
            host.with(move |host| host.set_tempo(tempo))
                .await
                .expect("Host tempo");
            let mut waited = 0;
            while host.session_grid().await.revision() == revision {
                assert!(
                    waited < consts::GRID_WAIT_BLOCKS,
                    "the Host publishes the {step} BPM grid"
                );
                render_blocks(&harness, 1).await;
                waited += 1;
            }
        }
        let (beat, downbeat) = beat_from(&host.session_grid().await, from);
        while host.position() <= beat + consts::SILENT_FOOT {
            render_blocks(&harness, 1).await;
        }
        beats.push((beat, downbeat));
    }
    render_blocks(&harness, consts::CLICK_TAIL_BLOCKS).await;
    harness.close().await;

    assert_eq!(
        (output.drops(), master.drops()),
        (0, 0),
        "the taps keep every frame"
    );
    let ride = Ride {
        output: output.drain(),
        master: master.drain(),
        start,
        beats,
    };
    assert_eq!(
        ride.output.len(),
        ride.master.len(),
        "both taps see every rendered frame"
    );
    ride
}

#[kithara::test(tokio)]
async fn the_metronome_clicks_on_every_host_beat_over_a_deck_through_a_tempo_ride(
    sine_440_long: Vec<f32>,
) {
    let tone: Vec<f32> = sine_440_long
        .into_iter()
        .step_by(usize::from(consts::CHANNELS))
        .collect();
    let tone_peak = peak(&tone);
    let half_tone = tone.iter().map(|sample| sample / 2.0).collect();
    let full = ride_over(tone).await;
    let half = ride_over(half_tone).await;

    if let Some(mut artifact) =
        AudioArtifactTap::from_env(&artifact_label(), consts::SAMPLE_RATE, consts::CHANNELS)
            .expect("listening artifact")
    {
        artifact.push(&full.output);
    }
    assert_eq!(
        peak(&full.master),
        tone_peak,
        "the deck plays the tone at its own level under the metronome"
    );
    assert_eq!(
        (half.start, &half.beats),
        (full.start, &full.beats),
        "a quieter deck rides the same Host beats"
    );
    assert!(
        half.master
            .iter()
            .copied()
            .eq(full.master.iter().map(|sample| sample / 2.0)),
        "the deck at half its level mixes to exactly half the master"
    );
    // WHY: Each output frame is the frame's mix under the duck plus the
    // click, and both rides duck and click alike. Twice the half ride's
    // output less the full ride's cancels the mix and leaves the click.
    let clicked: Vec<f32> = half
        .output
        .iter()
        .zip(&full.output)
        .map(|(half, full)| half.mul_add(2.0, -full))
        .collect();
    let heard = clicks(&clicked);
    assert_eq!(
        heard
            .iter()
            .map(|click| click.frame + full.start)
            .collect::<Vec<_>>(),
        full.beats
            .iter()
            .map(|(frame, _)| frame + consts::SILENT_FOOT)
            .collect::<Vec<_>>(),
        "one click rises from every Host beat of the ride, and nowhere else"
    );
    let long = heard
        .iter()
        .find(|click| click.frames > consts::CLICK_FRAMES);
    assert!(long.is_none(), "no ride click outlasts one click: {long:?}");
    assert_click_levels(&heard, &full.beats, consts::FULL_LEVEL);
}

#[kithara::test(tokio)]
async fn the_duck_under_a_click_keeps_a_loud_mix_at_or_under_the_limiter_ceiling() {
    let rate = NonZeroU32::new(consts::SAMPLE_RATE).expect("test sample rate");
    let session = HostConfig::offline(pools())
        .limiter(
            LimiterConfig::builder()
                .ceiling(consts::LOUD_CEILING)
                .build()
                .expect("limiter ceiling"),
        )
        .settings(
            HostSettings::builder()
                .sample_rate(rate)
                .metronome(
                    MetronomeConfig::builder()
                        .level(consts::LOUD_LEVEL)
                        .duck(consts::LOUD_DUCK)
                        .build(),
                )
                .build(),
        )
        .build();
    // WHY: A deck at twice the ceiling from its first frame, with no fade in,
    // holds the limited mix at the ceiling under the whole click. One block
    // plays before the taps attach and one more keeps the deck sounding past
    // the last rendered frame.
    let frames = u64::from(consts::BLOCK_FRAMES)
        * u64::try_from(consts::LOUD_BLOCKS + 2).expect("block count");
    let deck = vec![consts::DECK_LOUD; capacity(frames)];
    let options = OfflinePlayerOptions::builder()
        .crossfade_duration(0.0)
        .block_on_underrun(true)
        .build();
    let harness = play_resource(
        OfflinePlayer::with_options(options, session).await,
        move || {
            resource_from_reader(TestPcmReader::with_samples(
                AudioSpec::new(consts::CHANNELS, rate),
                deck,
            ))
        },
    )
    .await;
    let host = harness.host();
    let mut master = host
        .attach_tap(Tap::Master, capacity(frames))
        .await
        .expect("master tap");
    let mut output = host
        .attach_tap(Tap::Output, capacity(frames))
        .await
        .expect("output tap");
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    let tempo = tempo();
    host.with(move |host| host.set_tempo(tempo))
        .await
        .expect("Host tempo");
    let rendered = render_blocks(&harness, consts::LOUD_BLOCKS).await;
    harness.close().await;

    assert_eq!(
        (output.drops(), master.drops()),
        (0, 0),
        "the taps keep every frame"
    );
    let (output, master) = (output.drain(), master.drain());
    assert_eq!(
        output, rendered,
        "the output tap carries what graph_out plays"
    );
    let under_click: Vec<f32> = master
        .iter()
        .zip(&output)
        .filter(|(master, output)| master != output)
        .map(|(master, _)| master.abs())
        .collect();
    let quietest = under_click.iter().copied().fold(f32::INFINITY, f32::min);
    assert!(
        !under_click.is_empty()
            && quietest >= consts::LOUD_CEILING * consts::LOUD_SETTLED
            && peak(&master) <= consts::LOUD_CEILING,
        "the limited mix sits at the ceiling under the whole click: {quietest}"
    );
    let loudest = peak(&output);
    assert!(
        loudest <= consts::LOUD_CEILING * (1.0 + 4.0 * f32::EPSILON),
        "the ducked mix plus the click stays under the ceiling: {loudest}"
    );
}

/// The output and master taps of a player at the default Host config whose
/// deck holds `level` while the metronome clicks at [`consts::BPM`] for
/// [`consts::PAUSED_BLOCKS`] blocks.
async fn deck_under_clicks(level: f32) -> (Vec<f32>, Vec<f32>) {
    let rate = NonZeroU32::new(consts::SAMPLE_RATE).expect("test sample rate");
    // WHY: One block plays before the taps attach and one more keeps the
    // deck sounding past the last rendered frame.
    let deck_blocks = u64::try_from(consts::PAUSED_BLOCKS + 2).expect("block count");
    let frames = u64::from(consts::BLOCK_FRAMES) * deck_blocks;
    let deck = vec![level; usize::try_from(frames).expect("deck length")];
    let harness = play_resource(
        OfflinePlayer::with_sample_rate(
            OfflinePlayerOptions::builder().build(),
            consts::SAMPLE_RATE,
        )
        .await,
        move || {
            resource_from_reader(TestPcmReader::with_samples(
                AudioSpec::new(consts::CHANNELS, rate),
                deck,
            ))
        },
    )
    .await;
    let host = harness.host();
    let mut master = host
        .attach_tap(Tap::Master, capacity(frames))
        .await
        .expect("master tap");
    let mut output = host
        .attach_tap(Tap::Output, capacity(frames))
        .await
        .expect("output tap");
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    let tempo = tempo();
    host.with(move |host| host.set_tempo(tempo))
        .await
        .expect("Host tempo");
    render_blocks(&harness, consts::PAUSED_BLOCKS).await;
    harness.close().await;
    assert_eq!(
        (output.drops(), master.drops()),
        (0, 0),
        "the taps keep every frame"
    );
    (output.drain(), master.drain())
}

/// The samples of every channel of `pcm` over `frames`.
fn span(pcm: &[f32], frames: std::ops::Range<u64>) -> &[f32] {
    let channels = u64::from(consts::CHANNELS);
    let sample = |frame: u64| usize::try_from(frame * channels).expect("sample index");
    &pcm[sample(frames.start)..sample(frames.end)]
}

#[kithara::test(tokio)]
async fn a_full_duck_mutes_the_deck_through_the_hold_and_returns_it_over_the_release() {
    let (loud, loud_master) = deck_under_clicks(consts::DECK_LOUD).await;
    let (soft, soft_master) = deck_under_clicks(consts::DECK_SOFT).await;

    assert_eq!(
        (loud.len(), soft.len(), soft_master.len()),
        (loud_master.len(), loud_master.len(), loud_master.len()),
        "both runs render the same frames"
    );
    let channels = usize::from(consts::CHANNELS);
    let frames = u64::try_from(loud.len() / channels).expect("frame count");
    let ducked: Vec<f32> = loud
        .iter()
        .zip(&loud_master)
        .map(|(output, master)| output - master)
        .collect();
    // WHY: A click and its duck rise together from the silent foot on the
    // beat frame, so the output first leaves the master one frame after it.
    let beats: Vec<u64> = clicks(&ducked)
        .iter()
        .map(|click| click.frame - consts::SILENT_FOOT)
        .collect();
    assert!(
        beats.len() > 1,
        "the render holds several clicks: {beats:?}"
    );
    let hold_end = consts::CLICK_FRAMES + consts::HOLD_FRAMES;
    let release_end = hold_end + consts::RELEASE_FRAMES;
    let release_frames: f32 = consts::RELEASE_FRAMES.as_();
    let max_step = std::f32::consts::PI / (2.0 * release_frames);
    for (index, &beat) in beats.iter().enumerate() {
        let next = beats.get(index + 1).copied().unwrap_or(frames);
        assert!(
            beat + release_end <= next,
            "the duck of the beat on frame {beat} ends in the render before the next click"
        );

        // WHY: Both runs click the same beats with the same clicks, so where
        // their outputs agree while their decks differ, no deck reaches the
        // output.
        let held = beat + consts::ATTACK_FRAMES..beat + hold_end;
        assert!(
            span(&loud, held.clone()) == span(&soft, held.clone())
                && span(&loud_master, held.clone())
                    .iter()
                    .zip(span(&soft_master, held))
                    .all(|(loud, soft)| loud != soft),
            "from the peak of the click on frame {beat} through its hold no deck reaches the output"
        );

        let released = beat + hold_end..beat + release_end;
        let gains: Vec<f32> = span(&loud, released.clone())
            .iter()
            .zip(span(&soft, released.clone()))
            .zip(
                span(&loud_master, released.clone())
                    .iter()
                    .zip(span(&soft_master, released)),
            )
            .map(|((loud, soft), (loud_master, soft_master))| {
                (loud - soft) / (loud_master - soft_master)
            })
            .collect();
        for channel in 0..channels {
            let gains: Vec<f32> = gains
                .iter()
                .skip(channel)
                .step_by(channels)
                .copied()
                .collect();
            let (Some(first), Some(last)) = (gains.first(), gains.last()) else {
                panic!("the release spans frames");
            };
            assert!(
                first.abs() <= consts::GAIN_TOLERANCE && 1.0 - last <= max_step,
                "the release of the beat on frame {beat} returns the deck from silence to its level: {first} to {last}"
            );
            let jump = gains.windows(2).find(|pair| {
                pair[1] < pair[0] - consts::GAIN_TOLERANCE
                    || pair[1] - pair[0] > max_step + consts::GAIN_TOLERANCE
            });
            assert!(
                jump.is_none(),
                "the release of the beat on frame {beat} rises smoothly: {jump:?}"
            );
        }

        let after = beat + release_end..next;
        assert!(
            span(&loud, after.clone()) == span(&loud_master, after.clone())
                && span(&soft, after.clone()) == span(&soft_master, after),
            "after the release of the beat on frame {beat} the output is the master bit-exactly"
        );
    }
}

/// Renders `host` to the end of the click of Host beat
/// [`consts::LEVEL_BEATS`] and returns the output tap's take with the Host
/// beats inside it.
async fn render_to_the_level_beat(
    host: &OfflineHostHarness<TestPools>,
    tap: &mut TapProbe,
    mut take: Vec<f32>,
) -> (Vec<f32>, Vec<(u64, bool)>) {
    let grid = host.session_grid().await;
    let end = beat_frame(&grid, consts::LEVEL_BEATS) + consts::CLICK_FRAMES;
    host.render_forward(end - host.position()).await;
    take.extend(tap.drain());
    (take, host_beats(&grid, 0..end))
}

/// The clicks of `take` rise from the frame of every Host beat in `beats`,
/// and nowhere else.
fn assert_clicks_on(take: &[Click], beats: &[(u64, bool)]) {
    assert!(
        beats.len() > 1,
        "the take spans more than one beat: {beats:?}"
    );
    assert_eq!(
        take.iter().map(|click| click.frame).collect::<Vec<_>>(),
        beats
            .iter()
            .map(|(frame, _)| frame + consts::SILENT_FOOT)
            .collect::<Vec<_>>(),
        "one click rises from the frame of every Host beat, and nowhere else"
    );
}

#[kithara::test(tokio)]
async fn a_metronome_level_set_mid_click_sounds_from_the_next_click() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let (host, mut tap) = metronome_host(consts::SAMPLE_RATE, consts::BLOCKS * block).await;
    let bound = host.position() + consts::FIRST_CLICK_BLOCKS * block;
    let mut take = tap.drain();
    while clicks(&take).is_empty() && host.position() < bound {
        host.render_forward(consts::LEVEL_STEP_FRAMES).await;
        take.extend(tap.drain());
    }
    let heard = clicks(&take);
    let [first] = heard.as_slice() else {
        panic!("the first click sounds within a few blocks of the tempo: {heard:?}");
    };
    assert!(
        first.frame - consts::SILENT_FOOT + consts::ATTACK_FRAMES > host.position(),
        "the first click has not peaked when its level changes: {first:?}"
    );
    host.with(|host| host.metronome().set_level(consts::HALF_LEVEL))
        .await
        .expect("half the level");
    let (take, beats) = render_to_the_level_beat(&host, &mut tap, take).await;
    host.close().await;

    let heard = clicks(&take);
    assert_clicks_on(&heard, &beats);
    assert_click_levels(&heard[..1], &beats[..1], consts::FULL_LEVEL);
    assert_click_levels(&heard[1..], &beats[1..], consts::HALF_LEVEL);
}

/// Frames between consecutive clicks of `heard`.
fn click_spacings(heard: &[Click]) -> Vec<f64> {
    heard
        .windows(2)
        .map(|pair| {
            f64::from(u32::try_from(pair[1].frame - pair[0].frame).expect("a beat fits u32"))
        })
        .collect()
}

/// Frames of one beat at `bpm`.
fn beat_frames_at(bpm: f64) -> f64 {
    f64::from(consts::SAMPLE_RATE) * 60.0 / bpm
}

/// The take that closes the Host queue: a Host counting 120 BPM with its
/// metronome off through the first bar, switched on, its level halved after
/// the downbeat of the third bar, and its tempo moved to 128 BPM on a frame
/// halfway between two beats. Every click rises from a beat of the grid the
/// transport committed: the switch sounds from the next beat, the level from
/// the next click, the tempo from the frame it was set for.
#[kithara::test(tokio)]
async fn a_take_switches_the_metronome_on_halves_its_level_and_moves_the_tempo_on_a_frame() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let (host, mut tap) = counting_host(16 * consts::TAKE_BEAT_FRAMES).await;
    let mut artifact =
        AudioArtifactTap::from_env(&artifact_label(), consts::SAMPLE_RATE, consts::CHANNELS)
            .expect("listening artifact");
    let mut take = Vec::new();
    let mut record = |take: &mut Vec<f32>, pcm: Vec<f32>, label: &str| {
        if let Some(artifact) = artifact.as_mut() {
            artifact.push(&pcm);
            artifact.mark(label);
        }
        take.extend(pcm);
    };

    host.render_forward(block).await;
    let counted = host.session_grid().await;
    host.render_forward(beat_frame(&counted, consts::TAKE_ON_BEAT - 1) + block - host.position())
        .await;
    let on = host.position();
    record(&mut take, tap.drain(), "metronome on");
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");

    host.render_forward(
        beat_frame(&counted, consts::TAKE_LEVEL_BEAT - 1) + consts::CLICK_FRAMES + block
            - host.position(),
    )
    .await;
    record(&mut take, tap.drain(), "metronome level 0.5");
    host.with(|host| host.metronome().set_level(consts::HALF_LEVEL))
        .await
        .expect("half the level");

    let moved_on = beat_frame(&counted, consts::TAKE_TEMPO_BEAT) + consts::TAKE_BEAT_FRAMES / 2;
    let moved = Tempo::new(consts::TAKE_TO_BPM).expect("take tempo");
    let at = When::At(SessionFrame::new(
        i64::try_from(moved_on).expect("a take frame fits the session clock"),
    ));
    host.with(move |host| host.configure(HostSettingsChange::Tempo(moved), at))
        .await
        .expect("a frame beats ahead is reachable");
    host.render_forward(moved_on - host.position()).await;
    record(&mut take, tap.drain(), "tempo 128 BPM");

    let end = moved_on + consts::TAKE_MOVED_BEATS * consts::TAKE_BEAT_FRAMES + consts::CLICK_FRAMES;
    host.render_forward(end - host.position()).await;
    record(&mut take, tap.drain(), "end");
    let committed = host.session_grid().await;
    let tempo = host.with(|host| host.tempo()).await;
    host.close().await;

    assert_eq!(tap.drops(), 0, "the tap keeps every frame");
    assert_eq!(tempo, moved, "the Host reads the tempo it moved to");
    let beats: Vec<(u64, bool)> = host_beats(&counted, on..moved_on)
        .into_iter()
        .chain(host_beats(&committed, moved_on..end - consts::CLICK_FRAMES))
        .collect();
    let heard = clicks(&take);
    assert_clicks_on(&heard, &beats);
    let level_from = usize::try_from(consts::TAKE_LEVEL_BEAT - consts::TAKE_ON_BEAT)
        .expect("beats at full level");
    assert_click_levels(
        &heard[..level_from],
        &beats[..level_from],
        consts::FULL_LEVEL,
    );
    assert_click_levels(
        &heard[level_from..],
        &beats[level_from..],
        consts::HALF_LEVEL,
    );

    let straddle = usize::try_from(consts::TAKE_TEMPO_BEAT - consts::TAKE_ON_BEAT)
        .expect("beats before the tempo moves");
    let spacings = click_spacings(&heard);
    let counted_beat = beat_frames_at(consts::TAKE_FROM_BPM);
    let moved_beat = beat_frames_at(consts::TAKE_TO_BPM);
    assert!(
        spacings[..straddle]
            .iter()
            .all(|spacing| (spacing - counted_beat).abs() <= consts::BEAT_ROUNDING_FRAMES),
        "clicks before the move count 120 BPM: {spacings:?}"
    );
    assert!(
        (spacings[straddle] - (counted_beat + moved_beat) / 2.0).abs()
            <= consts::TEMPO_SMOOTH_FRAMES,
        "the beat the tempo moves inside runs half at 120 BPM and half at 128: {spacings:?}"
    );
    assert!(
        spacings[straddle + 1..]
            .iter()
            .all(|spacing| (spacing - moved_beat).abs() <= consts::BEAT_ROUNDING_FRAMES),
        "clicks after the move count 128 BPM: {spacings:?}"
    );
}

/// The session frame `frame` as a moment of the render clock.
fn at_frame(frame: u64) -> When<SessionFrame> {
    When::At(SessionFrame::new(
        i64::try_from(frame).expect("a test frame fits the session clock"),
    ))
}

/// A metronome switched on for a frame after a beat, inside that beat's
/// block, stays silent on the beat and sounds from the next one.
#[kithara::test(tokio)]
async fn a_metronome_switched_on_at_a_frame_inside_a_block_stays_silent_before_it() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let (host, mut tap) = counting_host(4 * consts::TAKE_BEAT_FRAMES).await;
    host.render_forward(block).await;
    let counted = host.session_grid().await;
    let beat = beat_frame(&counted, 1);
    let on = beat + consts::AFTER_BEAT_FRAMES;
    assert_eq!(
        beat / block,
        on / block,
        "the beat and the switch share a block"
    );
    let at = at_frame(on);
    host.with(move |host| {
        host.configure(
            HostSettingsChange::Metronome(MetronomeConfigChange::Enabled(true)),
            at,
        )
    })
    .await
    .expect("a frame ahead is reachable");

    let end = beat_frame(&counted, 3) + consts::CLICK_FRAMES;
    host.render_forward(end - host.position()).await;
    let take = tap.drain();
    host.close().await;

    assert_eq!(tap.drops(), 0, "the tap keeps every frame");
    assert_clicks_on(
        &clicks(&take),
        &host_beats(&counted, on..end - consts::CLICK_FRAMES + 1),
    );
}

/// Two tempo changes due in the block of beat 1, after it, leave the click
/// of that beat on the frame the tempo before them put it on; beats 2 and 3
/// follow the second change.
#[kithara::test(tokio)]
async fn two_tempo_changes_inside_one_block_keep_the_click_before_them_on_its_beat() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let (host, mut tap) = counting_host(4 * consts::TAKE_BEAT_FRAMES).await;
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    host.render_forward(block).await;
    let counted = host.session_grid().await;
    let beat = beat_frame(&counted, 1);
    let first = beat + consts::AFTER_BEAT_FRAMES;
    let second = first + consts::BETWEEN_CHANGES_FRAMES;
    assert_eq!(
        beat / block,
        second / block,
        "the beat and both changes share a block"
    );
    let (slow, fast) = (
        Tempo::new(consts::FIRST_CHANGE_BPM).expect("first tempo"),
        Tempo::new(consts::SECOND_CHANGE_BPM).expect("second tempo"),
    );
    let (first_at, second_at) = (at_frame(first), at_frame(second));
    host.with(move |host| {
        host.configure(HostSettingsChange::Tempo(slow), first_at)?;
        host.configure(HostSettingsChange::Tempo(fast), second_at)
    })
    .await
    .expect("frames ahead are reachable");

    host.render_forward(second + block - host.position()).await;
    let committed = host.session_grid().await;
    let last = beat_frame(&committed, 3);
    host.render_forward(last + consts::CLICK_FRAMES - host.position())
        .await;
    let take = tap.drain();
    host.close().await;

    assert_eq!(tap.drops(), 0, "the tap keeps every frame");
    let beats: Vec<(u64, bool)> = host_beats(&counted, 0..first)
        .into_iter()
        .chain((2..=3).map(|ordinal| {
            (
                beat_frame(&committed, ordinal),
                ordinal % consts::BEATS_PER_BAR == 0,
            )
        }))
        .collect();
    assert_clicks_on(&clicks(&take), &beats);
}

#[kithara::test(tokio)]
async fn a_refused_metronome_level_keeps_the_last_level() {
    let block = u64::from(consts::BLOCK_FRAMES);
    let (host, mut tap) = metronome_host(consts::SAMPLE_RATE, consts::BLOCKS * block).await;
    for level in [consts::OVER_LEVEL, 0.0] {
        let refused = host
            .with(move |host| host.metronome().set_level(level))
            .await;
        assert!(
            matches!(
                &refused,
                Err(PlayError::InvalidParameter { name, .. }) if name == "metronome_level"
            ),
            "a level of {level} is refused: {refused:?}"
        );
    }
    host.render_forward(block).await;
    let primed = tap.drain();
    let (take, beats) = render_to_the_level_beat(&host, &mut tap, primed).await;
    host.close().await;

    let heard = clicks(&take);
    assert_clicks_on(&heard, &beats);
    assert_click_levels(&heard, &beats, consts::FULL_LEVEL);
}

#[kithara::test(tokio)]
async fn a_host_refuses_a_metronome_config_out_of_its_bounds() {
    for (metronome, parameter) in [
        (
            MetronomeConfig::builder().level(consts::OVER_LEVEL).build(),
            "metronome_level",
        ),
        (
            MetronomeConfig::builder().duck(-0.01).build(),
            "metronome_duck",
        ),
    ] {
        let settings = HostSettings::builder().metronome(metronome).build();
        let refused =
            OfflineHostHarness::new(HostConfig::offline(pools()).settings(settings).build())
                .await
                .err();
        assert!(
            matches!(
                &refused,
                Some(PlayError::InvalidParameter { name, .. }) if name == parameter
            ),
            "a Host with {metronome:?} is refused naming {parameter}: {refused:?}"
        );
    }
}
