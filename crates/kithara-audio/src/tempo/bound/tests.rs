use std::num::NonZeroU32;

use kithara_bufpool::{ByteBudget, PcmPool};
use kithara_decode::{DecodeError, PcmChunk, PcmMeta, PcmSpec};
use kithara_events::PlaybackDirection;
use kithara_platform::sync::Arc;
use kithara_stretch::{ElasticConfig, ElasticEngine, ElasticSpanConfig, SignalsmithElastic};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

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
fn identity_map() -> TrackBeatMap {
    let markers: Vec<u64> = (0..8).map(|k| k * Consts::BEAT_FRAMES).collect();
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(120.0, markers, vec![0], Vec::new())),
        None,
        Consts::BEAT_FRAMES * 8,
        rate(),
    );
    TrackBeatMap::new(&analysis, rate()).expect("invariant: fixture markers form a map")
}

fn identity_grid() -> Arc<SessionAnchorCell> {
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
    anchor
}

fn identity_schedule() -> Arc<SourceSchedule> {
    let anchor = identity_grid();
    Arc::new(SourceSchedule::new(
        identity_map(),
        TrackBeat::default(),
        PlaybackDirection::Forward,
        anchor,
    ))
}

fn renderer_with(
    schedule: Arc<SourceSchedule>,
    pool: PcmPool,
) -> BoundRenderer<SignalsmithElastic> {
    renderer_with_origin(schedule, SessionBeat::default(), pool)
}

fn renderer_with_origin(
    schedule: Arc<SourceSchedule>,
    session_origin: SessionBeat,
    pool: PcmPool,
) -> BoundRenderer<SignalsmithElastic> {
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
    BoundRenderer::new(schedule, session_origin, engine, span_config, spec(), pool)
        .expect("invariant: the fixture grid is committed")
}

fn renderer() -> BoundRenderer<SignalsmithElastic> {
    renderer_with(identity_schedule(), PcmPool::default())
}

fn engine_retained_source_frames() -> u64 {
    let block = usize::try_from(BoundRenderer::<SignalsmithElastic>::BLOCK_FRAMES)
        .expect("invariant: the block fits usize");
    let config = ElasticConfig::try_from((
        Consts::RATE,
        usize::from(Consts::CHANNELS),
        block * 2,
        block,
    ))
    .expect("invariant: the fixture shape is representable");
    let latency = SignalsmithElastic::prepare(config)
        .expect("invariant: the engine prepares")
        .capabilities()
        .latency();
    latency
        .source_frames()
        .checked_add(latency.output_frames())
        .and_then(|frames| frames.to_u64())
        .expect("invariant: the retained fixture window fits u64")
}

fn chunk(frame_offset: u64) -> PcmChunk {
    chunk_with_frames(
        frame_offset,
        usize::try_from(Consts::CHUNK_FRAMES).expect("invariant: the chunk fits usize"),
    )
}

fn chunk_with_frames(frame_offset: u64, frames: usize) -> PcmChunk {
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

fn commit_tempo(anchor: &SessionAnchorCell, frame: i64, bpm: f64) {
    let frame = SessionFrame::new(frame);
    let beat = anchor
        .load()
        .expect("invariant: the fixture grid is committed")
        .beat_at(frame)
        .expect("invariant: the fixture commit frame is representable");
    anchor.publish(
        SessionAnchor::new(frame, beat, bpm / 60.0, rate())
            .expect("invariant: the fixture tempo is a positive rate"),
    );
}

#[kithara::test]
fn the_decks_presentation_frame_counts_the_output_frames_it_emitted() {
    let pool = PcmPool::with_byte_budget(1, 0, ByteBudget(0));
    let mut renderer = renderer_with(identity_schedule(), pool);

    let emitted = renderer
        .process(chunk(0))
        .expect("identity schedule must render")
        .map_or(0, |output| u64::from(output.meta.frames));

    assert_eq!(renderer.presentation_frame(), emitted);
}

#[kithara::test]
fn a_tempo_commit_inside_a_block_takes_effect_at_its_own_output_frame() {
    let anchor = identity_grid();
    let schedule = Arc::new(SourceSchedule::new(
        identity_map(),
        TrackBeat::default(),
        PlaybackDirection::Forward,
        Arc::clone(&anchor),
    ));
    let mut renderer = renderer_with(schedule, PcmPool::default());
    commit_tempo(&anchor, 256, 144.0);

    let emitted = renderer
        .process(chunk_with_frames(0, 563))
        .expect("the split span must stay inside the engine envelope")
        .map(|output| u64::from(output.meta.frames));

    assert_eq!(
        emitted,
        Some(BoundRenderer::<SignalsmithElastic>::BLOCK_FRAMES)
    );
}

#[kithara::test]
fn a_deck_bound_across_a_session_pause_plans_its_next_block_at_the_committed_slope() {
    let anchor = identity_grid();
    let mut renderer = renderer_with(
        Arc::new(SourceSchedule::new(
            identity_map(),
            TrackBeat::default(),
            PlaybackDirection::Forward,
            Arc::clone(&anchor),
        )),
        PcmPool::default(),
    );
    let _ = renderer
        .process(chunk(0))
        .expect("the deck must plan once before the pause");
    let before = renderer.elapsed_session_beats();
    let pause_frame = SessionFrame::new(512);
    let pause_beat = anchor
        .load()
        .expect("invariant: the fixture grid is committed")
        .beat_at(pause_frame)
        .expect("invariant: the pause boundary is representable");
    anchor.publish(
        SessionAnchor::new(pause_frame, pause_beat, Consts::SESSION_BPM / 60.0, rate())
            .expect("invariant: the paused anchor is valid"),
    );
    anchor.publish(
        SessionAnchor::new(SessionFrame::new(10_512), pause_beat, 144.0 / 60.0, rate())
            .expect("invariant: the resumed anchor is valid"),
    );

    let _ = renderer
        .process(chunk_with_frames(512, 700))
        .expect("the resumed block must use the committed slope");
    let planned = renderer.elapsed_session_beats() - before;
    let expected = 144.0 / 60.0 / f64::from(Consts::RATE)
        * BoundRenderer::<SignalsmithElastic>::BLOCK_FRAMES
            .to_f64()
            .expect("invariant: the block frame count is exact in f64");

    assert!((planned - expected).abs() < f64::EPSILON);
}

#[kithara::test]
fn two_decks_mid_block_at_different_offsets_apply_one_commit_at_the_same_output_frame() {
    let anchor = identity_grid();
    let second_origin = SessionBeat::new(
        100.0
            / Consts::BEAT_FRAMES
                .to_f64()
                .expect("invariant: fixture frame count is exact in f64"),
    )
    .expect("invariant: the second deck origin is a finite beat");
    let first = Arc::new(SourceSchedule::new(
        identity_map(),
        TrackBeat::default(),
        PlaybackDirection::Forward,
        Arc::clone(&anchor),
    ));
    let second = Arc::new(SourceSchedule::new(
        identity_map(),
        TrackBeat::new(f64::from(second_origin))
            .expect("invariant: the second track origin is a finite beat"),
        PlaybackDirection::Forward,
        Arc::clone(&anchor),
    ));
    let mut first = renderer_with(first, PcmPool::default());
    let mut second = renderer_with_origin(second, second_origin, PcmPool::default());
    commit_tempo(&anchor, 300, 144.0);

    assert!(
        second
            .process(chunk_with_frames(0, 100))
            .expect("the second deck must retain its preceding source")
            .is_none()
    );

    let _ = first
        .process(chunk_with_frames(0, 554))
        .expect("the first split span must render");
    let _ = second
        .process(chunk_with_frames(100, 574))
        .expect("the second split span must render");

    assert_eq!(
        (
            first.consumed_source_frames(),
            second.consumed_source_frames()
        ),
        (554, 574)
    );
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
        if let Some(output) = renderer
            .process(chunk(index * Consts::CHUNK_FRAMES))
            .expect("identity schedule must render")
        {
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
        if let Some(output) = renderer
            .process(chunk(index * Consts::CHUNK_FRAMES))
            .expect("unity schedule must render")
        {
            emitted += u64::from(output.meta.frames);
        }
    }

    assert_eq!(emitted, chunks * Consts::CHUNK_FRAMES);
}

/// A plan that reaches behind the bounded retained tail is a broken contract,
/// and it is reported as one rather than clamped to the oldest frame in hand.
#[kithara::test]
fn a_plan_behind_the_retained_source_is_typed() {
    let mut renderer = renderer();

    // The first chunk starts the pending window well past the schedule's
    // origin, so block zero asks for source the slot never saw.
    let error = renderer
        .process(chunk(Consts::BEAT_FRAMES))
        .expect_err("reaching behind the retained source must be typed");
    let DecodeError::PcmStream { source, .. } = error else {
        panic!("bound rendering failure must retain its typed source");
    };

    assert!(matches!(
        source.downcast_ref::<BoundError>(),
        Some(BoundError::BehindWindow { .. })
    ));
}

/// The declared hold never exceeds the source the slot has actually taken, so
/// the frontier it implies can only move forward.
#[kithara::test]
fn held_source_frames_never_exceeds_the_source_taken() {
    let mut renderer = renderer();
    let chunks = 4_u64;

    for index in 0..chunks {
        let _ = renderer
            .process(chunk(index * Consts::CHUNK_FRAMES))
            .expect("identity schedule must render");
    }

    assert!(renderer.held_source_frames() <= chunks * Consts::CHUNK_FRAMES);
}

/// The declared hold is the source window actually retained for an in-flight
/// re-prime: engine history plus one warmup span.
#[kithara::test]
fn held_source_frames_reports_the_retained_source_window() {
    let mut renderer = renderer();
    let retained = engine_retained_source_frames();
    let chunks = 16_u64;

    for index in 0..chunks {
        let _ = renderer
            .process(chunk(index * Consts::CHUNK_FRAMES))
            .expect("identity schedule must render");
    }

    assert_eq!(renderer.held_source_frames(), retained);
}

/// A reset drops every pending frame and the plan cursor with it, so a seek
/// does not splice the old source into the new position.
#[kithara::test]
fn reset_drops_pending_source() {
    let mut renderer = renderer();
    let _ = renderer
        .process(chunk(0))
        .expect("identity schedule must render");

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

    assert!(
        renderer
            .process(half)
            .expect("a partial identity span must accumulate")
            .is_none()
    );
}
