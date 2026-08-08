//! Two records at different tempos, placed on one session grid, must strike
//! their beats together. That is what a mix *is*, and it is the property the
//! rest of this module exists to serve — every other oracle here measures a
//! span or a count, none of them measure the sound.

use std::{f64::consts::TAU, num::NonZeroU32};

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_events::PlaybackDirection;
use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{BoundRenderer, bound_slot};
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
    const RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;
    /// A slow record.
    const SLOW_BPM: f64 = 96.0;
    /// A fast one. Neither matches the session, so both must stretch, in
    /// opposite directions.
    const FAST_BPM: f64 = 128.0;
    /// What the mix runs at.
    const SESSION_BPM: f64 = 120.0;
    /// Marker tones an octave apart, so the two decks are told apart by ear.
    const SLOW_HZ: f64 = 220.0;
    const FAST_HZ: f64 = 440.0;
    const SOURCE_SECS: f64 = 60.0;
    /// Beats of mix measured. Long enough that per-block rounding drift would
    /// accumulate past a frame if any existed.
    const MEASURED_BEATS: usize = 64;
    const CHUNK_FRAMES: usize = 512;
    /// Silence floor an onset must cross.
    const ONSET_FLOOR: f32 = 0.02;
    /// Where a ridden tempo ends up. Chosen so the slower record stays inside
    /// the engine's stretch envelope: 126/96 is 1.3125, under the 4/3 ceiling.
    const RAMP_TO: f64 = 126.0;
    /// Commits a ride is made of. Each is far longer than a planning block, so
    /// every deck samples the same one.
    const RIDE_STEPS: usize = 32;
    /// A hard sweep needs records near the middle of the stretch envelope, so
    /// both survive both ends of it. 110 and 130 keep the session free between
    /// 87 and 146 BPM; a 96 BPM record would cap the sweep at 128.
    const SWEEP_SLOW_BPM: f64 = 110.0;
    const SWEEP_FAST_BPM: f64 = 130.0;
    /// Where a hard sweep starts and ends: 90 to 145 is 55 BPM, and it is the
    /// widest both records take.
    const SWEEP_FROM: f64 = 90.0;
    const SWEEP_TO: f64 = 145.0;
    /// Seconds a hard sweep takes. Short enough to be a gesture, not a drift.
    const SWEEP_SECS: f64 = 10.0;
    /// A ride that deliberately overruns the envelope: 140/96 is 1.458.
    const RAMP_PAST_ENVELOPE: f64 = 140.0;
    /// Burst length as a fraction of a beat: short enough never to overlap.
    const BURST_OF_BEAT: f64 = 0.12;
}

/// The timbre a deck marks its beats with. Two decks summed are
/// indistinguishable by amplitude alone; a sine carries only its fundamental
/// and a square the odd harmonics above it, so the mix can be told apart by
/// ear and by spectrum.
#[derive(Clone, Copy)]
enum Pulse {
    Sine,
    Square,
}

impl Pulse {
    fn at(self, phase: f64) -> f64 {
        match self {
            Self::Sine => phase.sin(),
            Self::Square => {
                if phase.sin() >= 0.0 {
                    1.0
                } else {
                    -1.0
                }
            }
        }
    }
}

fn rate() -> NonZeroU32 {
    NonZeroU32::new(Consts::RATE).expect("invariant: fixture rate is non-zero")
}

fn spec() -> PcmSpec {
    PcmSpec::new(Consts::CHANNELS, rate())
}

/// A metronome as source audio: silence with one short burst on every beat of
/// its own tempo. The burst attacks on the first frame of the beat, so an
/// onset in the rendered output *is* where that beat landed — no analysis step
/// stands between the measurement and the claim.
fn pulse_track(bpm: f64, shape: Pulse, tone_hz: f64) -> (Vec<f32>, Vec<u64>) {
    let exact = |value: f64| {
        value
            .round()
            .to_u64()
            .expect("invariant: fixture magnitudes are small and positive")
    };
    let rate = f64::from(Consts::RATE);
    let frames = exact(rate * Consts::SOURCE_SECS);
    let beat_frames = exact(rate * 60.0 / bpm);
    let burst = exact(
        beat_frames
            .to_f64()
            .expect("invariant: fixture frame counts are exact in f64")
            * Consts::BURST_OF_BEAT,
    );
    let channels = usize::from(Consts::CHANNELS);
    let len = usize::try_from(frames).expect("invariant: the fixture fits memory") * channels;
    let mut pcm = vec![0.0_f32; len];
    for frame in 0..frames {
        let into_beat = frame % beat_frames;
        if into_beat >= burst {
            continue;
        }
        let decay = 1.0
            - into_beat
                .to_f64()
                .expect("invariant: fixture frame counts are exact in f64")
                / burst
                    .to_f64()
                    .expect("invariant: fixture frame counts are exact in f64");
        let phase = TAU
            * tone_hz
            * into_beat
                .to_f64()
                .expect("invariant: fixture frame counts are exact in f64")
            / rate;
        let value = shape.at(phase) * 0.6 * decay * decay;
        let at = usize::try_from(frame).expect("invariant: the fixture fits memory") * channels;
        for channel in 0..channels {
            pcm[at + channel] = value as f32;
        }
    }
    let markers = (0..frames.div_ceil(beat_frames))
        .map(|beat| beat * beat_frames)
        .collect();
    (pcm, markers)
}

/// The one grid both decks follow.
fn session_grid() -> Arc<SessionAnchorCell> {
    let cell = SessionAnchorCell::new();
    commit_tempo(&cell, Consts::SESSION_BPM);
    cell
}

fn commit_tempo(cell: &SessionAnchorCell, bpm: f64) {
    cell.publish(
        SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::default(),
            bpm / 60.0,
            rate(),
        )
        .expect("invariant: the fixture tempo is a positive rate"),
    );
}

/// How the session tempo moves while a deck renders, as a function of how far
/// into the mix the deck has played.
///
/// A ride of the pitch fader is not one commit but a stream of them, and the
/// deck reads the grid afresh for every block it plans. Keyed on **output**
/// position so both decks see the same ride: they consume source at different
/// rates, but they emit the same session.
///
/// Rides are quantized into [`Consts::RIDE_STEPS`] commits over the run.
/// Continuous here would be a fiction: a transport commit is a discrete event
/// scheduled to a block boundary, and every deck must see the same sequence of
/// them or they are following different grids.
type TempoRide = fn(f64) -> f64;

/// Holds still: the ordinary case.
fn steady(_progress: f64) -> f64 {
    Consts::SESSION_BPM
}

/// Rides from 120 up to 126 across the measured run — a hand on the fader,
/// not a step.
fn ramp_up(progress: f64) -> f64 {
    Consts::SESSION_BPM + (Consts::RAMP_TO - Consts::SESSION_BPM) * progress.clamp(0.0, 1.0)
}

/// A hard sweep: 90 up to 145 across the run. Both records are stretched hard
/// in both directions before it is over.
fn sweep(progress: f64) -> f64 {
    Consts::SWEEP_FROM + (Consts::SWEEP_TO - Consts::SWEEP_FROM) * progress.clamp(0.0, 1.0)
}

/// The same ride, pushed past what the stretch engine will do.
fn ramp_past_envelope(progress: f64) -> f64 {
    Consts::SESSION_BPM
        + (Consts::RAMP_PAST_ENVELOPE - Consts::SESSION_BPM) * progress.clamp(0.0, 1.0)
}

/// Renders one deck through the bound slot: its own tempo in, the session's
/// grid out.
fn render_deck(
    bpm: f64,
    shape: Pulse,
    tone_hz: f64,
    grid: &Arc<SessionAnchorCell>,
    frames: usize,
) -> Vec<f32> {
    render_deck_riding(bpm, shape, tone_hz, grid, frames, steady)
}

/// Both decks through one ride, driven in lockstep on one grid.
///
/// Rendering them one after the other would be a different experiment: a deck
/// stretched up consumes source more slowly, so each would cross the ride's
/// commits at its own pace and they would be following two rides, not one. A
/// session is one clock, so the fixture has to be one loop.
fn render_pair_riding(frames: usize, ride: TempoRide) -> (Vec<f32>, Vec<f32>) {
    render_pair_riding_at(Consts::SLOW_BPM, Consts::FAST_BPM, frames, ride)
}

fn render_pair_riding_at(
    slow_bpm: f64,
    fast_bpm: f64,
    frames: usize,
    ride: TempoRide,
) -> (Vec<f32>, Vec<f32>) {
    let grid = session_grid();
    let channels = usize::from(Consts::CHANNELS);
    let want = frames * channels;
    let mut slow = Deck::new(slow_bpm, Pulse::Sine, Consts::SLOW_HZ, &grid);
    let mut fast = Deck::new(fast_bpm, Pulse::Square, Consts::FAST_HZ, &grid);

    // Step by *session* time, not by source. A record stretched up eats its
    // source faster and emits less output per chunk, so feeding both the same
    // source would have them living in different parts of the mix. Each commit
    // covers the same stretch of session for both decks — which is the
    // readiness barrier a real transport applies before admitting one.
    for step in 1..=Consts::RIDE_STEPS {
        let progress = (step - 1)
            .to_f64()
            .expect("invariant: the step index is exact in f64")
            / Consts::RIDE_STEPS
                .to_f64()
                .expect("invariant: the step count is exact in f64");
        commit_tempo(&grid, ride(progress));
        let through = want * step / Consts::RIDE_STEPS;
        slow.render_through(through);
        fast.render_through(through);
    }
    slow.out.truncate(want);
    fast.out.truncate(want);
    (slow.out, fast.out)
}

/// One deck mid-render: its source, its slot, and what it has emitted.
struct Deck {
    source: Vec<f32>,
    slot: Box<dyn AudioEffect>,
    out: Vec<f32>,
    fed: usize,
}

impl Deck {
    fn new(bpm: f64, shape: Pulse, tone_hz: f64, grid: &Arc<SessionAnchorCell>) -> Self {
        let (source, markers) = pulse_track(bpm, shape, tone_hz);
        let source_frames = u64::try_from(source.len() / usize::from(Consts::CHANNELS))
            .expect("invariant: the fixture length fits u64");
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(bpm, markers, vec![0], Vec::new())),
            None,
            source_frames,
            rate(),
        );
        let map =
            TrackBeatMap::new(&analysis, rate()).expect("invariant: fixture markers form a map");
        let schedule = Arc::new(SourceSchedule::new(
            map,
            TrackBeat::default(),
            PlaybackDirection::Forward,
            Arc::clone(grid),
        ));
        Self {
            source,
            slot: bound_slot(schedule, spec(), PcmPool::default())
                .expect("invariant: an exact-span engine is compiled in"),
            out: Vec::new(),
            fed: 0,
        }
    }

    /// Feeds source until this deck has emitted `samples` of output, or its
    /// source runs out.
    fn render_through(&mut self, samples: usize) {
        let channels = usize::from(Consts::CHANNELS);
        while self.out.len() < samples
            && self.fed + Consts::CHUNK_FRAMES <= self.source.len() / channels
        {
            self.feed();
        }
    }

    fn feed(&mut self) {
        let channels = usize::from(Consts::CHANNELS);
        let slice = &self.source[self.fed * channels..(self.fed + Consts::CHUNK_FRAMES) * channels];
        let chunk = PcmChunk::new(
            PcmMeta {
                spec: spec(),
                frames: u32::try_from(Consts::CHUNK_FRAMES).expect("invariant: the chunk fits u32"),
                frame_offset: u64::try_from(self.fed).expect("invariant: the offset fits u64"),
                ..Default::default()
            },
            PcmPool::default().attach(slice.to_vec()),
        );
        self.fed += Consts::CHUNK_FRAMES;
        if let Some(rendered) = self.slot.process(chunk) {
            self.out.extend_from_slice(&rendered.samples);
        }
    }
}

fn render_deck_riding(
    bpm: f64,
    shape: Pulse,
    tone_hz: f64,
    grid: &Arc<SessionAnchorCell>,
    frames: usize,
    ride: TempoRide,
) -> Vec<f32> {
    let (source, markers) = pulse_track(bpm, shape, tone_hz);
    let source_frames = u64::try_from(source.len() / usize::from(Consts::CHANNELS))
        .expect("invariant: the fixture length fits u64");
    let analysis = TrackAnalysis::with_source_rate(
        Some(BeatGrid::new(bpm, markers, vec![0], Vec::new())),
        None,
        source_frames,
        rate(),
    );
    let map = TrackBeatMap::new(&analysis, rate()).expect("invariant: fixture markers form a map");
    let schedule = Arc::new(SourceSchedule::new(
        map,
        TrackBeat::default(),
        PlaybackDirection::Forward,
        Arc::clone(grid),
    ));
    let mut slot = bound_slot(schedule, spec(), PcmPool::default())
        .expect("invariant: an exact-span engine is compiled in");

    let channels = usize::from(Consts::CHANNELS);
    let want = frames * channels;
    let mut out: Vec<f32> = Vec::with_capacity(want);
    let mut fed = 0_usize;
    let available = usize::try_from(source_frames).expect("invariant: the fixture fits memory");
    while out.len() < want && fed + Consts::CHUNK_FRAMES <= available {
        let progress = (out.len() / channels)
            .to_f64()
            .expect("invariant: fixture frame counts are exact in f64")
            / frames
                .to_f64()
                .expect("invariant: fixture frame counts are exact in f64");
        commit_tempo(grid, ride(progress));
        let slice = &source[fed * channels..(fed + Consts::CHUNK_FRAMES) * channels];
        let chunk = PcmChunk::new(
            PcmMeta {
                spec: spec(),
                frames: u32::try_from(Consts::CHUNK_FRAMES).expect("invariant: the chunk fits u32"),
                frame_offset: u64::try_from(fed).expect("invariant: the offset fits u64"),
                ..Default::default()
            },
            PcmPool::default().attach(slice.to_vec()),
        );
        fed += Consts::CHUNK_FRAMES;
        if let Some(rendered) = slot.process(chunk) {
            out.extend_from_slice(&rendered.samples);
        }
    }
    out.truncate(want);
    out
}

/// Output frames the slot plans in one go, so one commit lands at most this
/// late on any deck.
const BLOCK_FRAMES: usize = 512;

/// Frames where a burst begins.
///
/// A refractory window is not a tolerance — it is what makes the measurement
/// mean "beat" at all. A square marker crosses the silence floor on every
/// cycle of its own tone, so a bare threshold counts oscillations, not
/// attacks. Nothing shorter than half a beat can be a second beat.
fn onsets(pcm: &[f32]) -> Vec<usize> {
    let channels = usize::from(Consts::CHANNELS);
    let refractory = session_beat_frames() / 2;
    let mut found: Vec<usize> = Vec::new();
    for (frame, samples) in pcm.chunks_exact(channels).enumerate() {
        let peak = samples.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));
        if peak <= Consts::ONSET_FLOOR {
            continue;
        }
        if found.last().is_none_or(|last| frame - last >= refractory) {
            found.push(frame);
        }
    }
    found
}

/// Output frames between session beats at the mix tempo.
fn session_beat_frames() -> usize {
    (f64::from(Consts::RATE) * 60.0 / Consts::SESSION_BPM)
        .round()
        .to_usize()
        .expect("invariant: one fixture beat fits usize")
}

fn decks(frames: usize) -> (Vec<f32>, Vec<f32>) {
    let grid = session_grid();
    (
        render_deck(
            Consts::SLOW_BPM,
            Pulse::Sine,
            Consts::SLOW_HZ,
            &grid,
            frames,
        ),
        render_deck(
            Consts::FAST_BPM,
            Pulse::Square,
            Consts::FAST_HZ,
            &grid,
            frames,
        ),
    )
}

/// Neither deck drifts from the session grid.
///
/// Distance is the discriminator, not a tolerance. A per-block rounding error
/// multiplies with the number of beats; a wobble in where a threshold crosses
/// a stretched attack does not. So the same span error is measured over a
/// quarter of the run and over all of it: drift would grow four-fold, jitter
/// stays put.
#[kithara::test]
fn neither_deck_drifts_from_the_session_grid() {
    let per_beat = session_beat_frames();
    let (slow, fast) = decks(per_beat * Consts::MEASURED_BEATS);

    for (name, rendered) in [("96 BPM", slow), ("128 BPM", fast)] {
        let beats = onsets(&rendered);
        assert!(beats.len() > 16, "{name}: the deck must strike repeatedly");
        let near = span_error(&beats, beats.len() / 4, per_beat);
        let far = span_error(&beats, beats.len() - 1, per_beat);

        assert!(
            far <= near.max(4) * 2,
            "{name}: the error grew with distance — {near} frames over {} beats, \
             {far} over {}; that is drift, not measurement",
            beats.len() / 4,
            beats.len() - 1
        );
    }
}

/// Frames between the first and `beats`-th onset, against what the session
/// grid puts there. The opening onset is skipped: a stretched attack crosses
/// the floor a few frames off its own edge, and that belongs to the
/// measurement rather than the grid.
fn span_error(beats: &[usize], count: usize, per_beat: usize) -> usize {
    let measured = &beats[1..=count];
    let span = measured[measured.len() - 1] - measured[0];
    span.abs_diff((measured.len() - 1) * per_beat)
}

/// The two decks do not drift apart. Same discriminator: a separation that
/// opens up grows with distance, one that merely wobbles does not.
#[kithara::test]
fn the_decks_do_not_drift_apart() {
    let per_beat = session_beat_frames();
    let (slow, fast) = decks(per_beat * Consts::MEASURED_BEATS);

    let slow_beats = onsets(&slow);
    let fast_beats = onsets(&fast);
    let pairs = slow_beats.len().min(fast_beats.len());
    assert!(pairs > 16, "both decks must strike repeatedly");

    let separation = |at: usize| fast_beats[at].abs_diff(slow_beats[at]) as i64;
    let opened_near = (separation(pairs / 4) - separation(1)).abs();
    let opened_far = (separation(pairs - 1) - separation(1)).abs();

    assert!(
        opened_far <= opened_near.max(4) * 2,
        "the decks drifted apart: separation opened by {opened_near} frames over {} beats \
         and {opened_far} over {}",
        pairs / 4,
        pairs - 1
    );
}

/// The separation they *do* hold is the engine's content delay, and it is not
/// the same for both decks.
///
/// An exact-span engine carries its algorithmic latency as delayed content,
/// declared in **source** frames. Two decks stretching by different ratios
/// therefore emerge offset from each other by a constant the slot never
/// compensates. It is not drift and it does not grow, but it is a flam, and a
/// mix cannot be called aligned while it stands. Pinned here at its measured
/// size so the compensation that removes it has something to move.
#[kithara::test]
fn the_uncompensated_engine_delay_offsets_the_decks() {
    let per_beat = session_beat_frames();
    let (slow, fast) = decks(per_beat * Consts::MEASURED_BEATS);

    let separation = onsets(&fast)[1].abs_diff(onsets(&slow)[1]);

    assert!(
        separation > 0,
        "the delay is uncompensated today; a zero here means it was fixed and \
         this oracle should become the alignment one"
    );
    assert!(
        separation < per_beat / 8,
        "the offset is a flam, not a missed beat: {separation} frames"
    );
}

/// A ride parts the decks, and the size of the parting is the commit
/// quantization.
///
/// `BoundRenderer` plans whole blocks. A tempo commit therefore takes effect
/// at the next block boundary rather than at the frame it was scheduled for —
/// up to 512 frames late, and late by a different amount on each deck, because
/// each is mid-block somewhere else. Under a steady tempo this never shows: it
/// is one boundary, once. Under a ride it is one per commit, and they add up.
///
/// `spec-v3.md` §2 requires the opposite — content computed as a function of
/// session beat, "splitting chunks at exact commit boundaries". Until the slot
/// can split a block, this is the cost, pinned at its measured size so the
/// split that removes it has something to change.
#[kithara::test]
fn a_tempo_ride_parts_the_decks_by_the_commit_quantization() {
    let frames = session_beat_frames() * Consts::MEASURED_BEATS;
    let (slow, fast) = render_pair_riding(frames, ramp_up);

    let slow_beats = onsets(&slow);
    let fast_beats = onsets(&fast);
    let pairs = slow_beats.len().min(fast_beats.len());
    assert!(pairs > 8, "both decks must strike through the ride");

    let separation = |at: usize| {
        i64::try_from(fast_beats[at].abs_diff(slow_beats[at]))
            .expect("invariant: the separation fits i64")
    };
    let opened = (separation(pairs - 1) - separation(1)).abs();

    assert!(
        opened > 0,
        "the parting is real today; a zero here means the slot learned to split \
         a block at a commit and this oracle should become the together one"
    );

    let budget = i64::try_from(Consts::RIDE_STEPS * BLOCK_FRAMES)
        .expect("invariant: the quantization budget fits i64");
    assert!(
        opened < budget,
        "the parting must stay inside one block per commit: {opened} frames \
         against a {budget}-frame budget over {} commits",
        Consts::RIDE_STEPS
    );
}

/// The ride is a ride: beats tighten monotonically as the tempo climbs, and
/// no boundary between commits shows up as a jump.
///
/// Measured as the spacing early against the spacing late. A step would put
/// one outsized gap between two normal ones; a ride puts the change in every
/// gap.
#[kithara::test]
fn a_tempo_ride_tightens_the_beat_without_a_jump() {
    let frames = session_beat_frames() * Consts::MEASURED_BEATS;
    let grid = session_grid();
    let rendered = render_deck_riding(
        Consts::SLOW_BPM,
        Pulse::Sine,
        Consts::SLOW_HZ,
        &grid,
        frames,
        ramp_up,
    );

    let beats = onsets(&rendered);
    assert!(beats.len() > 8, "the deck must strike through the ride");
    let spacings: Vec<usize> = beats.windows(2).map(|pair| pair[1] - pair[0]).collect();

    let early = spacings[1];
    let late = spacings[spacings.len() - 2];
    assert!(
        late < early,
        "a climbing tempo must tighten the beat: {early} frames early, {late} late"
    );

    let widest_step = spacings
        .windows(2)
        .map(|pair| pair[0].abs_diff(pair[1]))
        .max()
        .expect("invariant: the ride has consecutive spacings");
    let total = early.abs_diff(late);
    assert!(
        widest_step * 2 < total,
        "the change must be spread across the ride, not sat in one boundary: \
         widest single step {widest_step} against {total} overall"
    );
}

/// Ridden past the engine's stretch envelope, the slot drops blocks.
///
/// A record at 96 asked to keep up with 140 needs a ratio of 1.458 and the
/// engine stops at about 4/3. What happens then is not a typed refusal and not
/// a degradation: `render_available` fails, the block is logged and dropped,
/// and the deck loses audio. `spec-v3.md` §8.0 family 1 asks for a typed
/// reject at the envelope edge; this pins what actually happens so the reject
/// that replaces it has something to change.
#[kithara::test]
fn riding_past_the_stretch_envelope_drops_audio() {
    let frames = session_beat_frames() * Consts::MEASURED_BEATS;
    let grid = session_grid();

    let rendered = render_deck_riding(
        Consts::SLOW_BPM,
        Pulse::Sine,
        Consts::SLOW_HZ,
        &grid,
        frames,
        ramp_past_envelope,
    );

    let inside = render_deck_riding(
        Consts::SLOW_BPM,
        Pulse::Sine,
        Consts::SLOW_HZ,
        &session_grid(),
        frames,
        ramp_up,
    );

    assert!(
        onsets(&rendered).len() < onsets(&inside).len(),
        "beyond the envelope the deck loses beats rather than refusing the ride"
    );
}

/// A hard sweep — 90 to 145 BPM in ten seconds — moves the beat by the ratio
/// it names, and both records follow it there.
///
/// The gentle ride proves the mechanism; this proves the range. Both records
/// are stretched hard in both directions before it is over, and the spacing at
/// the end must be the spacing at the start scaled by 90/145.
#[kithara::test]
fn a_hard_sweep_moves_the_beat_by_the_ratio_it_names() {
    let frames = (f64::from(Consts::RATE) * Consts::SWEEP_SECS)
        .to_usize()
        .expect("invariant: the sweep length fits usize");
    let (slow, _) = render_pair_riding_at(
        Consts::SWEEP_SLOW_BPM,
        Consts::SWEEP_FAST_BPM,
        frames,
        sweep,
    );

    let beats = onsets(&slow);
    assert!(beats.len() > 8, "the sweep must carry many beats");
    let spacings: Vec<usize> = beats.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let early = spacings[1]
        .to_f64()
        .expect("invariant: a spacing is exact in f64");
    let late = spacings[spacings.len() - 2]
        .to_f64()
        .expect("invariant: a spacing is exact in f64");

    // The measured spacings sit one commit inside each end of the sweep, so
    // compare against the ratio the sweep actually reached there rather than
    // its endpoints.
    let reached = |at: usize| {
        sweep(
            at.to_f64().expect("invariant: the index is exact in f64")
                / spacings
                    .len()
                    .to_f64()
                    .expect("invariant: the count is exact in f64"),
        )
    };
    let expected = late / early;
    let named = reached(1) / reached(spacings.len() - 2);

    assert!(
        (expected - named).abs() < 0.06,
        "the beat must move by the ratio the sweep names: measured {expected:.3}, \
         named {named:.3}"
    );
}

/// The unbound reference: left alone, a 96 BPM record keeps its own spacing
/// and never matches the session's. Without this the oracles above would hold
/// just as well against a slot that does nothing.
#[kithara::test]
fn an_unstretched_record_keeps_its_own_spacing() {
    let per_beat = session_beat_frames();
    let (source, _) = pulse_track(Consts::SLOW_BPM, Pulse::Sine, Consts::SLOW_HZ);

    let beats = onsets(&source);
    let spacing = beats[1] - beats[0];

    assert!(
        spacing.abs_diff(per_beat) > 1,
        "an unstretched 96 BPM record must not already be spaced like 120 BPM"
    );
}

/// The mix carries both decks rather than cancelling them.
#[kithara::test]
fn the_mix_carries_both_decks() {
    let (slow, fast) = decks(session_beat_frames() * 4);
    let summed: Vec<f32> = slow.iter().zip(&fast).map(|(a, b)| (a + b) * 0.5).collect();
    let peak = |pcm: &[f32]| pcm.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));

    assert!(peak(&summed) > peak(&slow).max(peak(&fast)) * 0.4);
}

/// Writes the mix to disk so it can be judged by ear, which is the only judge
/// that matters for a mix.
///
/// Three files: each deck alone and the sum. The sine and the square are an
/// octave apart, so in the sum they are separable by ear — a flam between them
/// is heard as two strikes, and alignment as one.
#[kithara::test]
#[ignore = "diagnostic: writes /tmp/kithara_mix/*.wav for offline listening; run with --run-ignored"]
fn dump_the_mix_for_listening() {
    let dir = std::path::PathBuf::from("/tmp/kithara_mix");
    std::fs::create_dir_all(&dir).expect("invariant: the dump directory is writable");

    let frames = session_beat_frames() * Consts::MEASURED_BEATS;
    let (slow, fast) = decks(frames);
    let summed: Vec<f32> = slow.iter().zip(&fast).map(|(a, b)| (a + b) * 0.5).collect();

    for (name, pcm) in [
        ("01_deck_a_96bpm_sine.wav", &slow),
        ("02_deck_b_128bpm_square.wav", &fast),
        ("03_mix_on_a_120bpm_grid.wav", &summed),
    ] {
        write_wav(&dir.join(name), pcm);
    }

    // The same pair with a hand on the fader: 120 climbing to 126 across the
    // run, both decks following it.
    let (slow, fast) = render_pair_riding(frames, ramp_up);
    let ridden: Vec<f32> = slow.iter().zip(&fast).map(|(a, b)| (a + b) * 0.5).collect();
    write_wav(&dir.join("04_mix_riding_120_to_126.wav"), &ridden);

    // A hard sweep: 90 to 145 in ten seconds, on records at 110 and 130 so
    // both survive both ends of the stretch envelope.
    let sweep_frames = (f64::from(Consts::RATE) * Consts::SWEEP_SECS)
        .to_usize()
        .expect("invariant: the sweep length fits usize");
    let (slow, fast) = render_pair_riding_at(
        Consts::SWEEP_SLOW_BPM,
        Consts::SWEEP_FAST_BPM,
        sweep_frames,
        sweep,
    );
    let swept: Vec<f32> = slow.iter().zip(&fast).map(|(a, b)| (a + b) * 0.5).collect();
    write_wav(&dir.join("05_mix_sweeping_90_to_145.wav"), &swept);
}

/// Interleaved 16-bit PCM, the shape every player opens without asking.
#[cfg(test)]
fn write_wav(path: &std::path::Path, pcm: &[f32]) {
    use std::io::Write as _;

    let channels = u16::from(Consts::CHANNELS);
    let bits = 16_u16;
    let block_align = channels * bits / 8;
    let byte_rate = Consts::RATE * u32::from(block_align);
    let data_len = u32::try_from(pcm.len() * 2).expect("invariant: the dump fits a wav");

    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&Consts::RATE.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in pcm {
        let clipped = sample.clamp(-1.0, 1.0) * f32::from(i16::MAX);
        out.extend_from_slice(&(clipped as i16).to_le_bytes());
    }

    std::fs::File::create(path)
        .and_then(|mut file| file.write_all(&out))
        .expect("invariant: the dump is writable");
}
