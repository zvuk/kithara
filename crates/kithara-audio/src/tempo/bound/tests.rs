use std::num::NonZeroU32;

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_events::PlaybackDirection;
use kithara_platform::sync::Arc;
use kithara_stretch::{ElasticConfig, ElasticEngine, ElasticSpanConfig, SignalsmithElastic};
use kithara_test_utils::kithara;

use super::{BoundError, BoundRenderer};
use crate::{
    analysis::TrackAnalysis,
    musical::{
        SessionAnchor, SessionAnchorCell, SessionBeat, SessionFrame, SourceSchedule, TrackBeat,
        TrackBeatMap,
    },
    traits::AudioEffect,
    waveform::BeatGrid,
};

struct Consts;

impl Consts {
    const CHANNELS: u16 = 2;
    const RATE: u32 = 48_000;
    /// Frames between markers of the 120 BPM fixture.
    const BEAT_FRAMES: u64 = 24_000;
    /// Session tempo equal to the fixture track's own.
    const SESSION_BPM: f64 = 120.0;
    /// Frames carried by one fed chunk.
    const CHUNK_FRAMES: u64 = 512;
}

fn rate() -> NonZeroU32 {
    NonZeroU32::new(Consts::RATE).expect("invariant: fixture rate is non-zero")
}

fn spec() -> PcmSpec {
    PcmSpec::new(Consts::CHANNELS, rate())
}

/// Session tempo equal to the track's makes the schedule the identity on the
/// source axis, so a planned span is readable by eye: block `k` is source
/// `[k*BLOCK, (k+1)*BLOCK)`.
fn identity_schedule() -> Arc<SourceSchedule> {
    let markers: Vec<u64> = (0..8).map(|k| k * Consts::BEAT_FRAMES).collect();
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(120.0, markers, vec![0], Vec::new())),
        None,
        Consts::BEAT_FRAMES * 8,
        rate(),
    );
    let map = TrackBeatMap::new(&analysis, rate()).expect("invariant: fixture markers form a map");
    let anchor = SessionAnchorCell::new();
    anchor.publish(
        SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::default(),
            Consts::SESSION_BPM / 60.0,
            rate(),
        )
        .expect("invariant: the fixture tempo is a positive rate"),
    );
    Arc::new(SourceSchedule::new(
        map,
        TrackBeat::default(),
        PlaybackDirection::Forward,
        anchor,
    ))
}

fn renderer() -> BoundRenderer<SignalsmithElastic> {
    let block = usize::try_from(BoundRenderer::<SignalsmithElastic>::BLOCK_FRAMES)
        .expect("invariant: the block fits usize");
    let config = ElasticConfig::try_from((
        Consts::RATE,
        usize::from(Consts::CHANNELS),
        block * 2,
        block,
    ))
    .expect("invariant: the fixture shape is representable");
    let engine = SignalsmithElastic::prepare(config).expect("invariant: the engine prepares");
    let span_config = ElasticSpanConfig::try_from((1.0e-6, 0.5, 0.25))
        .expect("invariant: the fixture policy is finite and positive");
    BoundRenderer::new(
        identity_schedule(),
        engine,
        span_config,
        spec(),
        PcmPool::default(),
    )
}

fn engine_source_latency() -> usize {
    let block = usize::try_from(BoundRenderer::<SignalsmithElastic>::BLOCK_FRAMES)
        .expect("invariant: the block fits usize");
    let config = ElasticConfig::try_from((
        Consts::RATE,
        usize::from(Consts::CHANNELS),
        block * 2,
        block,
    ))
    .expect("invariant: the fixture shape is representable");
    SignalsmithElastic::prepare(config)
        .expect("invariant: the engine prepares")
        .capabilities()
        .latency()
        .source_frames()
}

fn chunk(frame_offset: u64) -> PcmChunk {
    let frames = usize::try_from(Consts::CHUNK_FRAMES).expect("invariant: the chunk fits usize");
    let samples = vec![0.25_f32; frames * usize::from(Consts::CHANNELS)];
    PcmChunk::new(
        PcmMeta {
            spec: spec(),
            frames: u32::try_from(frames).expect("invariant: the chunk fits u32"),
            frame_offset,
            ..Default::default()
        },
        PcmPool::default().attach(samples),
    )
}

/// The slot renders on the output axis: every chunk it emits is a whole number
/// of planned blocks, never a partial one and never the input's frame count
/// dressed up as output.
#[kithara::test]
fn emitted_chunks_are_whole_planned_blocks() {
    let mut renderer = renderer();
    let block = BoundRenderer::<SignalsmithElastic>::BLOCK_FRAMES;
    let mut emitted = 0_u64;

    for index in 0..8_u64 {
        if let Some(output) = renderer.process(chunk(index * Consts::CHUNK_FRAMES)) {
            let frames = u64::from(output.meta.frames);
            assert_eq!(
                frames % block,
                0,
                "an emitted chunk must be whole planned blocks, got {frames}"
            );
            emitted += frames;
        }
    }

    assert!(emitted > 0, "eight chunks of source must render something");
}

/// At unity the plan is the identity, so the slot renders one output frame per
/// source frame: an exact-span engine returns the whole span immediately and
/// carries its latency as content delay, not as a frame-count deficit.
#[kithara::test]
fn unity_plan_renders_one_output_frame_per_source_frame() {
    let mut renderer = renderer();
    let chunks = 8_u64;
    let mut emitted = 0_u64;

    for index in 0..chunks {
        if let Some(output) = renderer.process(chunk(index * Consts::CHUNK_FRAMES)) {
            emitted += u64::from(output.meta.frames);
        }
    }

    assert_eq!(emitted, chunks * Consts::CHUNK_FRAMES);
}

/// The slot retains nothing behind what it has consumed. A plan that reaches
/// back is a broken contract, and it is reported as one rather than clamped to
/// the oldest frame still in hand.
#[kithara::test]
fn a_plan_behind_the_retained_source_is_typed() {
    let mut renderer = renderer();

    // The first chunk starts the pending window well past the schedule's
    // origin, so block zero asks for source the slot never saw.
    renderer.process(chunk(Consts::BEAT_FRAMES));

    assert!(
        matches!(
            renderer.render_available(),
            Err(BoundError::BehindWindow { .. })
        ),
        "reaching behind the retained source must be typed, not clamped"
    );
}

/// The declared hold never exceeds the source the slot has actually taken, so
/// the frontier it implies can only move forward.
#[kithara::test]
fn held_source_frames_never_exceeds_the_source_taken() {
    let mut renderer = renderer();
    let chunks = 4_u64;

    for index in 0..chunks {
        renderer.process(chunk(index * Consts::CHUNK_FRAMES));
    }

    assert!(renderer.held_source_frames() <= chunks * Consts::CHUNK_FRAMES);
}

/// The hold reaches the engine's declared source latency: content for those
/// frames has not left the engine, so the frontier must stay behind them even
/// though their output frames were already emitted.
#[kithara::test]
fn held_source_frames_reaches_the_declared_latency() {
    let mut renderer = renderer();
    let latency = u64::try_from(engine_source_latency()).expect("invariant: latency fits u64");
    let chunks = 16_u64;

    for index in 0..chunks {
        renderer.process(chunk(index * Consts::CHUNK_FRAMES));
    }

    assert_eq!(renderer.held_source_frames(), latency);
}

/// A reset drops every pending frame and the plan cursor with it, so a seek
/// does not splice the old source into the new position.
#[kithara::test]
fn reset_drops_pending_source() {
    let mut renderer = renderer();
    renderer.process(chunk(0));

    renderer.reset();

    assert_eq!(renderer.held_source_frames(), 0);
}

/// The bound slot never forwards source. It emits whole planned blocks or
/// nothing, so a half block yields nothing yet — exactly where the streaming
/// slot at unity would have passed the chunk through verbatim.
#[kithara::test]
fn a_partial_block_of_source_emits_nothing() {
    let mut renderer = renderer();
    let frames = usize::try_from(BoundRenderer::<SignalsmithElastic>::BLOCK_FRAMES / 2)
        .expect("invariant: half a block fits usize");
    let half = PcmChunk::new(
        PcmMeta {
            spec: spec(),
            frames: u32::try_from(frames).expect("invariant: half a block fits u32"),
            ..Default::default()
        },
        PcmPool::default().attach(vec![0.25_f32; frames * usize::from(Consts::CHANNELS)]),
    );

    assert!(renderer.process(half).is_none());
}
