//! A decoder's source clock change ends the outgoing source by the true-EOF
//! law and starts the incoming source at the phase its last output left.
//! A source command during or after that cut starts the incoming source on
//! its own entry; a control change keeps the phase.

use std::num::NonZeroU64;

use kithara_audio::WaitingReason;
use kithara_bufpool::PoolRegion;
use kithara_command::Sender;
use kithara_signal::{AudioChunkInfo, ExponentialApproach};
use kithara_warp::{Pitch, WarpConfig, WarpControls, WarpControlsChange};

use super::{doubles::*, *};

const RATE: u32 = 44_100;
/// Sample rate of an incoming source whose decoder changed the format too.
const FASTER_RATE: u32 = 48_000;
/// First frame of the incoming source, far from the outgoing coordinates.
const NEXT: u64 = 50_000;
/// First frame of a third source after a second clock change.
const LATER: u64 = 90_000;
const INCOMING_FRAMES: u32 = 16_384;
/// Outgoing frames of the interleaving lanes: at 1.25 the last output before
/// them sits at 102.5 and the next one at 103.75, inside the first quantum.
const OUTGOING_FRAMES: u64 = 103;
const QUANTUM: usize = 128;
/// Lane frame of a command that lands between two packets of a keylocked cut.
const MID_DRAIN: u64 = 256;
/// A value no ramp below reaches, on the last real outgoing frame.
const IMPULSE: f32 = -30_000.0;

/// Common denominator of every output interval below.
const SCALE: u128 = 8;

/// Outgoing frames left when the decoder changes its source clock.
#[derive(Clone, Copy, Debug)]
enum Staged {
    /// The lane fed its last quantum whole.
    None,
    /// The prepared quantum lacks this many frames.
    ShortOf(usize),
    /// Exactly this many frames.
    Exactly(usize),
}

/// One decoded source in the order the lane renders it.
#[derive(Clone, Copy, Debug)]
struct Source {
    start: u64,
    /// Frames before the next clock change; the last source runs on.
    frames: Option<u64>,
    /// Output interval in source frames, as `step / SCALE`.
    step: u128,
    /// Whether its first output keeps the phase the previous source left.
    carries: bool,
}

/// What the outgoing source holds on its last real frame.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tail {
    Ramp,
    Impulse,
}

fn stereo() -> AudioSpec {
    stereo_at(RATE)
}

fn stereo_at(rate: u32) -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(rate).expect("test sample rate"))
}

fn ramp_value(frame: u64) -> f32 {
    f32::from(u16::try_from(frame % 4096).expect("ramp value"))
}

/// Stereo PCM of source frames `frame..frame + frames` under `revision`.
fn pcm(
    pools: &PoolRegion<TestPools>,
    (revision, spec): (u64, AudioSpec),
    frame: u64,
    frames: u64,
    value: impl Fn(u64) -> f32,
) -> AudioChunk {
    let samples = (frame..frame + frames)
        .flat_map(|frame| {
            let value = value(frame);
            [value, value]
        })
        .collect::<Vec<_>>();
    let frames = u32::try_from(frames).expect("chunk frames");
    let mut chunk = chunk_with_frames(pools, spec, frame, frames, &samples);
    chunk.meta.source_revision = revision;
    chunk
}

/// Stereo ramp of source frames `frame..frame + frames` under `revision`.
fn ramp(pools: &PoolRegion<TestPools>, revision: u64, frame: u64, frames: u64) -> AudioChunk {
    pcm(pools, (revision, stereo()), frame, frames, ramp_value)
}

/// A decoder whose source clock changes with its stamp revision; it prepares
/// the format of its latest stamp ahead of the render and may end.
struct ClockSource {
    stamp: SourceDiscontinuity,
    chunks: VecDeque<AudioChunk>,
    ended: bool,
}

impl AudioSource for ClockSource {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn seek_for_render(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        self.seek(target)
    }

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn discontinuity(&self) -> Option<SourceDiscontinuity> {
        Some(self.stamp)
    }

    fn prepare_deferred(&mut self) -> Option<AudioSpec> {
        Some(*self.stamp.spec())
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        match self.chunks.pop_front() {
            Some(chunk) => TrackStep::Produced(Fetch::data(chunk)),
            None if self.ended => TrackStep::Eof,
            None => TrackStep::Blocked(WaitingReason::Waiting),
        }
    }
}

/// One emitted lane packet.
struct Output {
    meta: AudioChunkInfo,
    samples: Vec<f32>,
}

/// What one lane step did.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Emitted,
    Blocked,
    Changed,
    Ended,
}

struct Lane {
    source: WarpSource<ClockSource, TestPools>,
    sender: Sender<LaneProtocol>,
    emitted: Vec<Output>,
}

impl Lane {
    fn new(pools: &PoolRegion<TestPools>, speed: f32, keylock: bool, quantum: usize) -> Self {
        let raw = ClockSource {
            stamp: SourceDiscontinuity::new(1, stereo()),
            chunks: VecDeque::new(),
            ended: false,
        };
        let (sender, inbox) = channel::<LaneProtocol>(ChannelConfig::builder().build());
        let config = WarpConfig::builder()
            .speed(speed)
            .maybe_controls(keylock.then(|| WarpControls::builder().pitch(Pitch::HELD).build()))
            .render_quantum_frames(NonZeroUsize::new(quantum).expect("test quantum"))
            .build();
        let renderer = kithara_warp::Warp::new((), &config).renderer(stereo(), pools.clone());
        let source = WarpSource::new(
            raw,
            renderer,
            stereo(),
            pools.clone(),
            LaneSetup {
                inbox,
                preload_chunks: NonZeroUsize::MIN,
                declick: consts::DEFAULT_DECLICK,
            },
        );
        Self {
            source,
            sender,
            emitted: Vec::new(),
        }
    }

    fn push(&mut self, chunk: AudioChunk) {
        self.source.source.chunks.push_back(chunk);
    }

    /// The decoder publishes `revision` before any of its PCM.
    fn publish(&mut self, revision: u64, spec: AudioSpec) {
        self.source.source.stamp = SourceDiscontinuity::new(revision, spec);
    }

    /// The decoder moves to `revision`, whose source starts at `start`.
    fn change_clock(&mut self, pools: &PoolRegion<TestPools>, revision: u64, start: u64) {
        self.publish(revision, stereo());
        self.push(ramp(pools, revision, start, u64::from(INCOMING_FRAMES)));
    }

    fn end(&mut self) {
        self.source.source.ended = true;
    }

    fn staged(&self) -> usize {
        self.source
            .render_input
            .as_ref()
            .map_or(0, |input| input.len() / 2)
    }

    fn send(&mut self, command: LaneCommand) {
        self.sender
            .send(When::Next, command_batch(command))
            .expect("the lane has room for the command");
    }

    fn send_at(&mut self, frame: u64, command: LaneCommand) {
        let at = LaneFrame {
            segment: self.source.cursor().segment,
            frame,
        };
        self.sender
            .send(When::At(at), command_batch(command))
            .expect("the lane has room for the command");
    }

    fn metas(&self) -> impl Iterator<Item = &AudioChunkInfo> {
        self.emitted.iter().map(|output| &output.meta)
    }

    /// Every sample `revision` emitted, in order.
    fn samples(&self, revision: u64) -> Vec<f32> {
        self.emitted
            .iter()
            .filter(|output| output.meta.source_revision == revision)
            .flat_map(|output| output.samples.iter().copied())
            .collect()
    }

    /// One lane step under a scheduler that prepares deferred decoder work
    /// after each of them; every packet continues its lane segment.
    fn step(&mut self) -> Result<Step, TrackFailureKind> {
        let step = match self.source.step_track() {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                assert_eq!(data.samples.len(), data.frames() * 2);
                assert!(data.samples.iter().all(|sample| sample.is_finite()));
                let frame = self
                    .emitted
                    .last()
                    .filter(|last| last.meta.segment == data.meta.segment)
                    .map_or(0, |last| last.meta.lane_frame + u64::from(last.meta.frames));
                assert_eq!(
                    data.meta.lane_frame,
                    frame,
                    "packet {} continues its lane segment",
                    self.emitted.len()
                );
                self.emitted.push(Output {
                    meta: data.meta,
                    samples: data.samples.to_vec(),
                });
                Step::Emitted
            }
            TrackStep::Failed(failure) => return Err(failure),
            TrackStep::Eof => {
                assert!(
                    self.source.source.ended,
                    "a source clock change is not the end of the track"
                );
                Step::Ended
            }
            TrackStep::Blocked(_) => Step::Blocked,
            TrackStep::Produced(_) | TrackStep::StateChanged => Step::Changed,
        };
        flush_deferred(&mut self.source);
        Ok(step)
    }
}

/// Steps `lane` until it ends or blocks twice in a row.
#[kithara::hang_watchdog]
fn run(lane: &mut Lane) -> Result<(), TrackFailureKind> {
    let mut blocked = 0;
    for _ in 0..16_384 {
        hang_tick!();
        match lane.step()? {
            Step::Emitted => {
                blocked = 0;
                hang_reset!();
            }
            Step::Blocked => {
                blocked += 1;
                if blocked == 2 {
                    return Ok(());
                }
            }
            Step::Ended => return Ok(()),
            Step::Changed => {}
        }
    }
    panic!("the lane never blocked");
}

/// Steps `lane` until it emitted `outputs` frames in all or blocks.
#[kithara::hang_watchdog]
fn run_until(lane: &mut Lane, outputs: usize) -> Result<(), TrackFailureKind> {
    for _ in 0..64 {
        hang_tick!();
        if positions(lane.metas()).len() >= outputs || lane.step()? == Step::Blocked {
            return Ok(());
        }
    }
    Ok(())
}

/// Exact source position of every output frame of `metas`, reduced.
fn positions<'a>(metas: impl IntoIterator<Item = &'a AudioChunkInfo>) -> Vec<(u128, u128)> {
    metas
        .into_iter()
        .flat_map(|meta| {
            let span = meta.source_span.expect("rendered source span");
            assert_eq!(span.output_frames(), u64::from(meta.frames));
            (0..span.output_frames()).map(move |frame| {
                let (numerator, denominator) = span.source_ratio_at(frame).expect("position");
                reduced(numerator, denominator.get())
            })
        })
        .collect()
}

fn reduced(numerator: u128, denominator: u128) -> (u128, u128) {
    let (mut a, mut b) = (numerator, denominator);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    (numerator / a, denominator / a)
}

/// Positions of the first `outputs` frames over `sources`: each source emits
/// every output before its end, and the next one starts at the phase past it
/// or, without a carry, on its own first frame.
fn law(sources: &[Source], outputs: usize) -> Vec<(u128, u128)> {
    let mut phase = 0;
    let mut positions = Vec::new();
    for source in sources {
        if !source.carries {
            phase = 0;
        }
        let end = source.frames.map(|frames| u128::from(frames) * SCALE);
        while positions.len() < outputs && end.is_none_or(|end| phase < end) {
            positions.push(reduced(u128::from(source.start) * SCALE + phase, SCALE));
            phase += source.step;
        }
        if let Some(end) = end {
            phase -= end;
        }
    }
    positions
}

/// Outputs `source` emits before its end.
fn outputs_of(source: Source) -> usize {
    law(
        &[Source {
            carries: false,
            ..source
        }],
        usize::MAX,
    )
    .len()
}

/// At most `count` scaled positions from `from` by `step`, all below `end`.
fn walk(from: u128, step: u128, end: Option<u128>, count: usize) -> Vec<u128> {
    (0..)
        .map(|output| from + output * step)
        .take_while(|&position| end.is_none_or(|end| position < end))
        .take(count)
        .collect()
}

fn assert_law(lane: &Lane, outcome: Result<(), TrackFailureKind>, expected: &[(u128, u128)]) {
    let actual = positions(lane.metas());
    let diverged = actual
        .iter()
        .zip(expected)
        .position(|(actual, expected)| actual != expected);
    assert!(
        outcome.is_ok() && actual.len() >= expected.len() && diverged.is_none(),
        "outcome={outcome:?}: first divergence at output {diverged:?} \
         (actual {:?}, expected {:?}); emitted {} of {} expected outputs",
        diverged.and_then(|frame| actual.get(frame)),
        diverged.and_then(|frame| expected.get(frame)),
        actual.len(),
        expected.len(),
    );
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|sample| sample.to_bits()).collect()
}

/// A lane holding `staged` of its outgoing source, whose last real frame
/// carries `tail`; returns it with that source's physical end.
fn outgoing_lane(
    pools: &PoolRegion<TestPools>,
    (speed, keylock, quantum): (f32, bool, usize),
    staged: Staged,
    tail: Tail,
) -> (Lane, usize) {
    let mut lane = Lane::new(pools, speed, keylock, quantum);
    lane.push(ramp(pools, 1, 0, 1));
    run(&mut lane).expect("the outgoing source prepares");
    let prepared = lane
        .source
        .prepared_frames
        .expect("the lane prepares its first quantum");
    let end = match staged {
        Staged::None => prepared,
        Staged::ShortOf(frames) => prepared - frames,
        Staged::Exactly(frames) => frames,
    };
    let last = u64::try_from(end - 1).expect("frames");
    lane.push(pcm(pools, (1, stereo()), 1, last, |frame| {
        if tail == Tail::Impulse && frame == last {
            IMPULSE
        } else {
            ramp_value(frame)
        }
    }));
    run(&mut lane).expect("the outgoing source renders");
    (lane, end)
}

#[kithara::test]
#[case::faster_overhang(1.25, 10, false, 128, Staged::ShortOf(3))]
#[case::keylock_overhang(1.25, 10, true, 128, Staged::ShortOf(3))]
#[case::slower_overhang(0.625, 5, false, 126, Staged::ShortOf(1))]
#[case::slower_zero_staged(0.75, 6, false, 128, Staged::None)]
#[case::within_the_boundary(1.25, 10, false, 128, Staged::Exactly(103))]
fn a_source_clock_change_emits_every_outgoing_sample_and_carries_its_phase(
    #[case] speed: f32,
    #[case] step: u128,
    #[case] keylock: bool,
    #[case] quantum: usize,
    #[case] staged: Staged,
) {
    let pools = pools();
    let (mut lane, end) = outgoing_lane(&pools, (speed, keylock, quantum), staged, Tail::Ramp);
    let left = lane.staged();
    let prepared = lane.source.prepared_frames;
    let past_boundary = u128::try_from(end).expect("end") * SCALE
        > u128::try_from(quantum).expect("quantum") * step;
    let geometry = match staged {
        Staged::None => left == 0 && past_boundary,
        Staged::ShortOf(_) => left == end && past_boundary,
        Staged::Exactly(_) => left == end,
    };
    assert!(
        geometry,
        "{staged:?}: left={left} end={end} prepared={prepared:?}"
    );

    lane.change_clock(&pools, 2, NEXT);
    let outcome = run(&mut lane);

    let outgoing = Source {
        start: 0,
        frames: Some(u64::try_from(end).expect("end")),
        step,
        carries: false,
    };
    let incoming = Source {
        start: NEXT,
        frames: None,
        step,
        carries: true,
    };
    let expected = law(&[outgoing, incoming], outputs_of(outgoing) + quantum);
    assert_law(&lane, outcome, &expected);
}

#[kithara::test]
#[case::faster_overhang(1.25, 10, false, Staged::ShortOf(3))]
#[case::keylock_overhang(1.25, 10, true, Staged::ShortOf(3))]
#[case::slower_zero_staged(0.75, 6, false, Staged::None)]
fn a_source_cut_renders_the_terminal_pcm_and_leaks_nothing_into_the_incoming_source(
    #[case] speed: f32,
    #[case] step: u128,
    #[case] keylock: bool,
    #[case] staged: Staged,
) {
    let pools = pools();
    let geometry = (speed, keylock, QUANTUM);
    let mut outgoing = Vec::new();
    let mut incoming = Vec::new();
    for tail in [Tail::Ramp, Tail::Impulse] {
        let (mut reference, end) = outgoing_lane(&pools, geometry, staged, tail);
        reference.end();
        run(&mut reference).expect("the reference source ends");
        let (mut lane, cut_end) = outgoing_lane(&pools, geometry, staged, tail);
        assert_eq!(cut_end, end);
        lane.publish(2, stereo());
        lane.push(pcm(
            &pools,
            (2, stereo()),
            NEXT,
            u64::from(INCOMING_FRAMES),
            |frame| -ramp_value(frame) - 1.0,
        ));
        let outcome = run(&mut lane);

        let source = Source {
            start: 0,
            frames: Some(u64::try_from(end).expect("end")),
            step,
            carries: false,
        };
        let next = Source {
            start: NEXT,
            frames: None,
            step,
            carries: true,
        };
        assert_law(
            &lane,
            outcome,
            &law(&[source, next], outputs_of(source) + 1),
        );
        assert_eq!(
            positions(reference.metas()),
            law(&[source], usize::MAX),
            "{tail:?}: the true EOF emits every output before the physical end and no more"
        );
        let terminal = reference.samples(1);
        assert_eq!(
            bits(&lane.samples(1)),
            bits(&terminal),
            "{tail:?}: the cut renders the true-EOF PCM of its source"
        );
        outgoing.push(terminal);
        incoming.push(lane.samples(2));
    }
    assert_ne!(
        bits(&outgoing[0]),
        bits(&outgoing[1]),
        "the last real outgoing frame sounds in its terminal PCM"
    );
    assert!(!incoming[0].is_empty());
    assert_eq!(
        bits(&incoming[0]),
        bits(&incoming[1]),
        "no outgoing PCM reaches the incoming source"
    );
}

/// The lane change a player sends after the outgoing tail and before the
/// decoder's incoming source is adopted.
#[derive(Clone, Copy, Debug)]
enum Between {
    HostRate,
    Segment,
    Unity,
    Keylock,
}

#[kithara::test]
#[case::host_rate(Between::HostRate)]
#[case::segment(Between::Segment)]
#[case::unity(Between::Unity)]
#[case::keylock(Between::Keylock)]
fn a_lane_change_between_the_tail_and_the_adoption_keeps_the_clock_law(#[case] between: Between) {
    let pools = pools();
    let mut lane = Lane::new(&pools, 1.25, false, QUANTUM);
    lane.push(ramp(&pools, 1, 0, OUTGOING_FRAMES));
    run(&mut lane).expect("the outgoing source stages");
    assert_eq!(
        lane.staged(),
        usize::try_from(OUTGOING_FRAMES).expect("frames")
    );
    let outgoing = Source {
        start: 0,
        frames: Some(OUTGOING_FRAMES),
        step: 10,
        carries: false,
    };

    lane.publish(2, stereo());
    let tail = run_until(&mut lane, outputs_of(outgoing));
    let next = SegmentId::FIRST.next();
    let (command, step, carries) = match between {
        Between::HostRate => (
            LaneCommand::SetHostRate {
                id: next,
                rate: NonZeroU32::new(RATE).expect("rate"),
            },
            10,
            false,
        ),
        Between::Segment => (
            LaneCommand::Segment {
                id: next,
                from: stereo().duration_for(NEXT).expect("entry"),
                speed: SpeedCurve::Constant(1.25),
            },
            10,
            false,
        ),
        Between::Unity => (LaneCommand::SetSpeed(SpeedCurve::Constant(1.0)), 8, true),
        Between::Keylock => (
            LaneCommand::Warp(WarpControlsChange::Pitch(Pitch::HELD)),
            10,
            true,
        ),
    };
    lane.send(command);
    lane.change_clock(&pools, 2, NEXT);
    let outcome = tail.and_then(|()| run(&mut lane));

    let incoming = Source {
        start: NEXT,
        frames: None,
        step,
        carries,
    };
    let expected = law(&[outgoing, incoming], outputs_of(outgoing) + QUANTUM);
    assert_law(&lane, outcome, &expected);
}

/// Where a command reaches a source cut.
#[derive(Clone, Copy, Debug)]
enum Point {
    /// Between two outgoing packets of a long keylocked cut.
    MidDrain,
    /// After the adoption, before the first incoming window.
    AfterAdoption,
}

/// The command a player sends at that point.
#[derive(Clone, Copy, Debug)]
enum During {
    /// A host rate change whose decoder keeps the published stamp.
    HostRate,
    /// A host rate change whose decoder publishes its own revision later.
    LateHostRate,
    Segment,
    Jump,
    /// A speed control, as an output interval in `1 / SCALE` frames.
    Speed(u8),
}

#[kithara::test]
#[case::mid_drain_host_rate(Point::MidDrain, During::HostRate)]
#[case::mid_drain_late_host_rate(Point::MidDrain, During::LateHostRate)]
#[case::mid_drain_segment(Point::MidDrain, During::Segment)]
#[case::mid_drain_jump(Point::MidDrain, During::Jump)]
#[case::mid_drain_speed(Point::MidDrain, During::Speed(12))]
#[case::mid_drain_unity(Point::MidDrain, During::Speed(8))]
#[case::adopted_host_rate(Point::AfterAdoption, During::HostRate)]
#[case::adopted_late_host_rate(Point::AfterAdoption, During::LateHostRate)]
#[case::adopted_segment(Point::AfterAdoption, During::Segment)]
#[case::adopted_jump(Point::AfterAdoption, During::Jump)]
#[case::adopted_unity(Point::AfterAdoption, During::Speed(8))]
fn a_source_command_supersedes_a_cut_and_a_control_keeps_its_phase(
    #[case] point: Point,
    #[case] during: During,
) {
    let pools = pools();
    let keylock = matches!(point, Point::MidDrain);
    let (mut lane, end) = outgoing_lane(
        &pools,
        (1.25, keylock, QUANTUM),
        Staged::ShortOf(3),
        Tail::Ramp,
    );
    let end = u128::try_from(end).expect("end") * SCALE;
    let next = SegmentId::FIRST.next();
    let command = match during {
        During::HostRate | During::LateHostRate => LaneCommand::SetHostRate {
            id: next,
            rate: NonZeroU32::new(RATE).expect("rate"),
        },
        During::Segment => LaneCommand::Segment {
            id: next,
            from: stereo().duration_for(NEXT).expect("entry"),
            speed: SpeedCurve::Constant(1.25),
        },
        During::Jump => LaneCommand::Jump {
            to: stereo().duration_for(LATER).expect("jump target"),
        },
        During::Speed(step) => LaneCommand::SetSpeed(SpeedCurve::Constant(f32::from(step) / 8.0)),
    };
    lane.publish(2, stereo());
    if matches!(point, Point::MidDrain) {
        lane.send_at(MID_DRAIN, command.clone());
    }
    let cut = run(&mut lane);
    let at = match point {
        Point::MidDrain => usize::try_from(MID_DRAIN).expect("frame"),
        Point::AfterAdoption => {
            lane.send(command);
            positions(lane.metas()).len()
        }
    };
    let served = match during {
        During::LateHostRate => {
            let served = run(&mut lane);
            lane.publish(3, stereo());
            lane.push(ramp(&pools, 3, LATER, u64::from(INCOMING_FRAMES)));
            cut.and(served)
        }
        During::Jump => {
            if matches!(point, Point::AfterAdoption) {
                lane.push(ramp(&pools, 2, NEXT, u64::from(INCOMING_FRAMES)));
            }
            lane.push(ramp(&pools, 2, LATER, u64::from(INCOMING_FRAMES)));
            cut
        }
        During::HostRate | During::Segment | During::Speed(_) => {
            lane.push(ramp(&pools, 2, NEXT, u64::from(INCOMING_FRAMES)));
            cut
        }
    };
    let outcome = served.and_then(|()| run(&mut lane));

    let entry = |start: u64| u128::from(start) * SCALE;
    let declick = lane.source.declick_frames().get();
    let from = u128::try_from(at).expect("frame") * 10;
    let carried = |step: u128| {
        let tail = walk(from, step, Some(end), usize::MAX);
        let next = tail.last().map_or(from, |last| last + step);
        (tail, next - end)
    };
    let mut expected = walk(0, 10, Some(end), at);
    match (point, during) {
        (_, During::HostRate | During::Segment) => {
            expected.extend(walk(entry(NEXT), 10, None, QUANTUM));
        }
        (_, During::LateHostRate) => expected.extend(walk(entry(LATER), 10, None, QUANTUM)),
        (Point::MidDrain, During::Jump) => {
            expected.extend(walk(from, 10, Some(end), declick));
            expected.extend(walk(entry(LATER), 10, None, QUANTUM));
        }
        (Point::AfterAdoption, During::Jump) => {
            let (_, residual) = carried(10);
            expected.extend(walk(entry(NEXT) + residual, 10, None, declick));
            expected.extend(walk(entry(LATER), 10, None, QUANTUM));
        }
        (Point::MidDrain, During::Speed(step)) => {
            let (tail, residual) = carried(u128::from(step));
            expected.extend(tail);
            expected.extend(walk(entry(NEXT) + residual, step.into(), None, QUANTUM));
        }
        (Point::AfterAdoption, During::Speed(step)) => {
            let (_, residual) = carried(10);
            expected.extend(walk(entry(NEXT) + residual, step.into(), None, QUANTUM));
        }
    }
    let expected = expected
        .into_iter()
        .map(|position| reduced(position, SCALE))
        .collect::<Vec<_>>();
    assert_law(&lane, outcome, &expected);
}

#[kithara::test]
#[case::faster(12)]
#[case::unity(8)]
fn a_control_between_tail_packets_keeps_the_outgoing_format_of_a_faster_clock(#[case] step: u8) {
    let pools = pools();
    let (mut lane, end) = outgoing_lane(
        &pools,
        (1.25, true, QUANTUM),
        Staged::ShortOf(3),
        Tail::Ramp,
    );
    let end = u128::try_from(end).expect("end") * SCALE;
    lane.publish(2, stereo_at(FASTER_RATE));
    lane.send_at(
        MID_DRAIN,
        LaneCommand::SetSpeed(SpeedCurve::Constant(f32::from(step) / 8.0)),
    );
    let cut = run(&mut lane);
    lane.push(pcm(
        &pools,
        (2, stereo_at(FASTER_RATE)),
        NEXT,
        u64::from(INCOMING_FRAMES),
        |frame| -ramp_value(frame) - 1.0,
    ));
    let outcome = cut.and_then(|()| run(&mut lane));

    let at = usize::try_from(MID_DRAIN).expect("frame");
    let from = u128::try_from(at).expect("frame") * 10;
    let step = u128::from(step);
    let mut expected = walk(0, 10, Some(end), at);
    let tail = walk(from, step, Some(end), usize::MAX);
    let residual = tail.last().map_or(from, |last| last + step) - end;
    let mut expected = expected
        .drain(..)
        .chain(tail)
        .map(|position| reduced(position, SCALE))
        .collect::<Vec<_>>();
    let scale = SCALE * u128::from(RATE);
    expected.push(reduced(
        u128::from(NEXT) * scale + residual * u128::from(FASTER_RATE),
        scale,
    ));
    assert_law(&lane, outcome, &expected);
}

#[kithara::test]
#[case::above_then_below_half(298)]
#[case::above_then_whole(301)]
fn repeated_clock_changes_carry_each_tail_phase(#[case] second: u64) {
    let pools = pools();
    let mut lane = Lane::new(&pools, 1.25, false, QUANTUM);
    lane.push(ramp(&pools, 1, 0, OUTGOING_FRAMES));
    run(&mut lane).expect("the first source stages");
    lane.publish(2, stereo());
    lane.push(ramp(&pools, 2, NEXT, second));
    let middle = run(&mut lane);
    lane.change_clock(&pools, 3, LATER);
    let outcome = middle.and_then(|()| run(&mut lane));

    let sources = [
        Source {
            start: 0,
            frames: Some(OUTGOING_FRAMES),
            step: 10,
            carries: false,
        },
        Source {
            start: NEXT,
            frames: Some(second),
            step: 10,
            carries: true,
        },
        Source {
            start: LATER,
            frames: None,
            step: 10,
            carries: true,
        },
    ];
    let expected = law(&sources, law(&sources[..2], usize::MAX).len() + QUANTUM);
    assert_law(&lane, outcome, &expected);
}

/// A carried half frame on keylocked unity: the whole incoming window of 129
/// frames holds one output past its 128-output quantum, and the next cut
/// finds no staged frame yet still owes it.
#[kithara::test]
fn a_keylocked_unity_cut_emits_the_output_its_half_phase_leaves() {
    const WHOLE: u64 = 129;
    let pools = pools();
    let mut lane = Lane::new(&pools, 1.5, true, QUANTUM);
    lane.push(ramp(&pools, 1, 0, OUTGOING_FRAMES));
    run(&mut lane).expect("the first source stages");
    lane.publish(2, stereo());
    let first = run(&mut lane);
    lane.send(LaneCommand::SetSpeed(SpeedCurve::Constant(1.0)));
    lane.push(ramp(&pools, 2, NEXT, WHOLE));
    let second = first.and_then(|()| run(&mut lane));
    assert_eq!(lane.staged(), 0, "the incoming window renders whole");
    lane.change_clock(&pools, 3, LATER);
    let outcome = second.and_then(|()| run(&mut lane));

    let sources = [
        Source {
            start: 0,
            frames: Some(OUTGOING_FRAMES),
            step: 12,
            carries: false,
        },
        Source {
            start: NEXT,
            frames: Some(WHOLE),
            step: 8,
            carries: true,
        },
        Source {
            start: LATER,
            frames: None,
            step: 8,
            carries: true,
        },
    ];
    let expected = law(&sources, law(&sources[..2], usize::MAX).len() + QUANTUM);
    assert_law(&lane, outcome, &expected);
}

/// An approach law crossing its segment boundary inside the cut ends the
/// outgoing source as its true EOF does; a decoder that also raises its
/// sample rate starts at that phase on its own clock.
#[kithara::test]
#[case::short(Staged::ShortOf(3))]
#[case::zero_staged(Staged::None)]
fn an_approach_cut_crosses_its_segment_boundary_into_a_faster_clock(#[case] staged: Staged) {
    const BOUNDARY: u64 = 132;
    let pools = pools();
    let approach = |initial, target| {
        ExponentialApproach::new(initial, target, 64.0).expect("finite approach segment")
    };
    let curve = SpeedCurve::ApproachSteps(
        vec![(0, approach(1.25, 1.1)), (BOUNDARY, approach(1.1, 1.4))].into(),
    );
    let mut ends = Vec::new();
    let mut lanes: [Lane; 2] = std::array::from_fn(|_| {
        let mut lane = Lane::new(&pools, 1.25, false, QUANTUM);
        lane.send(LaneCommand::SetSpeed(curve.clone()));
        lane
    });
    for lane in &mut lanes {
        lane.push(ramp(&pools, 1, 0, 1));
        run(lane).expect("the outgoing source prepares");
        let prepared = lane
            .source
            .prepared_frames
            .expect("the lane prepares its first quantum");
        let end = match staged {
            Staged::ShortOf(frames) => prepared - frames,
            Staged::None | Staged::Exactly(_) => prepared,
        };
        lane.push(ramp(&pools, 1, 1, u64::try_from(end - 1).expect("frames")));
        run(lane).expect("the outgoing source renders");
        ends.push(end);
    }
    assert_eq!(ends[0], ends[1]);
    let [mut reference, mut lane] = lanes;
    reference.end();
    run(&mut reference).expect("the reference source ends");
    let terminal = positions(reference.metas());
    assert!(
        terminal.len() > usize::try_from(BOUNDARY).expect("boundary"),
        "the terminal outputs cross the segment boundary"
    );

    lane.publish(2, stereo_at(FASTER_RATE));
    lane.push(pcm(
        &pools,
        (2, stereo_at(FASTER_RATE)),
        NEXT,
        u64::from(INCOMING_FRAMES),
        ramp_value,
    ));
    let outcome = run(&mut lane);

    let last = reference
        .metas()
        .last()
        .and_then(|meta| meta.source_span)
        .expect("terminal span");
    let (numerator, denominator) = last
        .source_ratio_at(last.output_frames())
        .expect("first unpublished output");
    let denominator = denominator.get();
    let residual = numerator
        .checked_sub(u128::try_from(ends[0]).expect("end") * denominator)
        .expect("the first unpublished output starts at or past the physical end");
    let first = reduced(
        u128::from(NEXT) * denominator * u128::from(RATE) + residual * u128::from(FASTER_RATE),
        denominator * u128::from(RATE),
    );
    let mut expected = terminal;
    expected.push(first);
    assert_law(&lane, outcome, &expected);
}

/// Exact source position of output `k` of a handover.
type Handover = fn(u128) -> (u128, u128);

/// Source position of output `k` of a 1.25 → 1.0 handover over 8 outputs
/// from source frame 160: a step at 1.125, or a linear ramp whose position
/// is `160 + 1.25 k - k² / 64`.
fn stepped_handover(k: u128) -> (u128, u128) {
    reduced(160 * SCALE + 9 * k, SCALE)
}

fn ramped_handover(k: u128) -> (u128, u128) {
    reduced(160 * 64 + 80 * k - k * k, 64)
}

#[kithara::test]
#[case::varispeed(false, SpeedCurve::Constant(1.0), None)]
#[case::keylock(true, SpeedCurve::Constant(1.0), None)]
#[case::varispeed_steps(
    false,
    SpeedCurve::Steps(vec![(0, 1.125), (8, 1.0)].into()),
    Some(stepped_handover as Handover)
)]
#[case::varispeed_ramp(
    false,
    SpeedCurve::Ramp { to: 1.0, frames: NonZeroU64::new(8).expect("ramp") },
    Some(ramped_handover as Handover)
)]
#[case::keylock_steps(
    true,
    SpeedCurve::Steps(vec![(0, 1.125), (8, 1.0)].into()),
    Some(stepped_handover as Handover)
)]
#[case::keylock_ramp(
    true,
    SpeedCurve::Ramp { to: 1.0, frames: NonZeroU64::new(8).expect("ramp") },
    Some(ramped_handover as Handover)
)]
fn a_unity_handover_inside_a_cut_drains_the_outgoing_tail(
    #[case] keylock: bool,
    #[case] curve: SpeedCurve,
    #[case] handover: Option<Handover>,
) {
    let pools = pools();
    let mut lane = Lane::new(&pools, 1.25, keylock, QUANTUM);
    let at = u64::try_from(QUANTUM).expect("frame");
    lane.send_at(at, LaneCommand::SetSpeed(curve));
    let frames = 200;
    lane.push(ramp(&pools, 1, 0, frames));
    let outgoing = run(&mut lane);
    lane.change_clock(&pools, 2, NEXT);
    let outcome = outgoing.and_then(|()| run(&mut lane));

    let entry = handover.map_or_else(Vec::new, |position| (0..8).map(position).collect());
    let unity = u128::from(at) * 10 + if handover.is_some() { 72 } else { 0 };
    let expected = walk(0, 10, None, QUANTUM)
        .into_iter()
        .map(|position| reduced(position, SCALE))
        .chain(entry)
        .chain(
            walk(unity, 8, Some(u128::from(frames) * SCALE), usize::MAX)
                .into_iter()
                .chain(walk(u128::from(NEXT) * SCALE, 8, None, QUANTUM))
                .map(|position| reduced(position, SCALE)),
        )
        .collect::<Vec<_>>();
    assert_law(&lane, outcome, &expected);
}
