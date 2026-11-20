use std::num::{NonZeroU128, NonZeroUsize};

use kithara_signal::{AudioChunkInfo, ExponentialApproach, SourceSpan};
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;

use super::*;
use crate::{
    render::trajectory::Fraction,
    test_pools::{TestPools, sample_buffer},
};

const QUANTUM: usize = 128;
const CUTS: [(u64, u32); 4] = [
    (144_384, 44_100),
    (143_360, 44_100),
    (150_017, 48_000),
    (149_000, 44_100),
];

fn spec(rate: u32) -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(rate).expect("sample rate"))
}

fn fraction((numerator, denominator): (u128, NonZeroU128)) -> Fraction {
    Fraction::new(numerator, denominator.get()).expect("source coordinate")
}

fn exact(value: Fraction) -> (u128, u128) {
    (value.numerator, value.denominator.get())
}

fn renderer(curve: SpeedCurve) -> WarpRenderer<TestPools> {
    let config = WarpConfig::builder()
        .render_quantum_frames(NonZeroUsize::new(QUANTUM).expect("quantum"))
        .build();
    let (_, rate) = CUTS[0];
    let mut renderer =
        crate::Warp::new((), &config).renderer(spec(rate), crate::test_pools::pools());
    renderer.set_speed(curve, 1).expect("speed law");
    renderer.prepare(spec(rate));
    renderer
}

fn meta(start: u64, rate: u32, revision: u64) -> AudioChunkInfo {
    AudioChunkInfo {
        spec: spec(rate),
        frame_offset: start,
        source_revision: revision,
        ..Default::default()
    }
}

fn starts(output: &AudioChunk) -> Vec<(u128, u128)> {
    let span = output.meta.source_span.expect("physical output span");
    (0..span.output_frames())
        .map(|frame| exact(fraction(span.source_ratio_at(frame).expect("output start"))))
        .collect()
}

/// Feeds the next source window of `meta`, cut to its first `staged` frames
/// when given, and returns the window's law, the fed end and its output starts.
fn feed(
    renderer: &mut WarpRenderer<TestPools>,
    meta: AudioChunkInfo,
    staged: Option<usize>,
) -> (SourceSpan, u64, Vec<(u128, u128)>) {
    let mut frames = renderer
        .prepare_source_window(meta, 65_536, QUANTUM)
        .expect("source window")
        .get();
    let law = renderer
        .mapping_span(meta.frame_offset, meta.spec.sample_rate, 2 * QUANTUM)
        .expect("source law");
    if let Some(staged) = staged {
        frames = renderer
            .prepare_terminal_quantum(staged)
            .expect("the outgoing tail prepares")
            .get();
    }
    let input = AudioChunk::new(
        AudioChunkInfo {
            frames: u32::try_from(frames).expect("fed frames"),
            ..meta
        },
        sample_buffer(&renderer.pools, &vec![0.25; frames * 2]),
    );
    let output = renderer
        .render_quantum(input)
        .continue_value()
        .expect("prepared input");
    let end = meta.frame_offset + u64::try_from(frames).expect("fed frames");
    (law, end, output.as_ref().map(starts).unwrap_or_default())
}

/// Drains a source cut to `revision` and returns the starts it emits.
#[kithara::hang_watchdog]
fn cut(renderer: &mut WarpRenderer<TestPools>, revision: u64) -> Vec<(u128, u128)> {
    let mut emitted = Vec::new();
    loop {
        hang_tick!();
        // The stage services the renderer before every bounded step.
        renderer.prepare(renderer.spec);
        let Some(output) = renderer
            .drain_source_cut(QUANTUM, revision)
            .expect("the outgoing tail drains")
        else {
            break;
        };
        assert!(output.frames() <= QUANTUM, "one bounded packet per call");
        emitted.extend(starts(&output));
    }
    emitted
}

/// Output starts of `law` before the physical `end`, and the source phase the
/// next output leaves past it.
fn expected(law: SourceSpan, end: u64) -> (Vec<(u128, u128)>, Fraction) {
    let starts = (0..law.output_frames())
        .map(|frame| fraction(law.source_ratio_at(frame).expect("law start")))
        .take_while(|start| start.numerator / start.denominator.get() < u128::from(end))
        .collect::<Vec<_>>();
    let count = u64::try_from(starts.len()).expect("output count");
    assert!(count < law.output_frames(), "the law reaches past the cut");
    let residual = fraction(law.source_ratio_at(count).expect("next start"))
        .sub(Fraction::new(u128::from(end), 1).expect("physical end"))
        .expect("the next start lies at or past the physical end");
    (starts.into_iter().map(exact).collect(), residual)
}

fn carried(residual: Fraction, (rate, next, next_rate): (u32, u64, u32)) -> (u128, u128) {
    exact(
        Fraction::new(
            residual.numerator * u128::from(next_rate),
            residual.denominator.get() * u128::from(rate),
        )
        .and_then(|carried| carried.add(Fraction::new(u128::from(next), 1)?))
        .expect("incoming coordinate"),
    )
}

fn first_start(renderer: &mut WarpRenderer<TestPools>, meta: AudioChunkInfo) -> (u128, u128) {
    renderer
        .prepare_source_window(meta, 65_536, QUANTUM)
        .expect("source window");
    let span = renderer
        .prepared_quantum
        .expect("prepared quantum")
        .source_span;
    exact(fraction(span.source_ratio_at(0).expect("first start")))
}

#[kithara::test]
#[case::unity_below_lattice(SpeedCurve::Constant(1.0), (24, 25), Some(64))]
#[case::unity_above_lattice(SpeedCurve::Constant(1.0), (1, 4), Some(64))]
#[case::slower_below_lattice(SpeedCurve::Constant(0.75), (24, 25), Some(64))]
#[case::faster_above_lattice(SpeedCurve::Constant(1.25), (1, 4), Some(64))]
#[case::approach(
    SpeedCurve::Approach(ExponentialApproach::new(1.0, 0.9, 220.5).expect("approach")),
    (1, 4),
    Some(64)
)]
#[case::entry_beyond_the_end(SpeedCurve::Constant(1.0), (5, 4), Some(1))]
#[case::whole_unity_window(SpeedCurve::Constant(1.0), (1, 4), None)]
#[case::whole_faster_window(SpeedCurve::Constant(1.25), (1, 4), None)]
#[case::whole_approach_window(
    SpeedCurve::Approach(ExponentialApproach::new(1.0, 0.9, 220.5).expect("approach")),
    (1, 4),
    None
)]
fn a_source_cut_drains_to_the_physical_end_and_carries_the_exact_phase(
    #[case] curve: SpeedCurve,
    #[case] phase: (u128, u128),
    #[case] staged: Option<usize>,
) {
    let mut renderer = renderer(curve);
    let (start, rate) = CUTS[0];
    renderer
        .reanchor_source(
            meta(start, rate, 0),
            (
                u128::from(start) * phase.1 + phase.0,
                NonZeroU128::new(phase.1).expect("phase denominator"),
            ),
        )
        .expect("fractional entry phase");

    for (revision, pair) in (0..).zip(CUTS.windows(2)) {
        let [(start, rate), (next, next_rate)] = [pair[0], pair[1]];
        let (law, end, mut emitted) = feed(&mut renderer, meta(start, rate, revision), staged);
        emitted.extend(cut(&mut renderer, revision + 1));
        let (starts, residual) = expected(law, end);
        assert_eq!(
            emitted, starts,
            "cut at {start}: every output that starts before source end {end}, and no other"
        );
        assert!(renderer.carried_phase.is_some());
        renderer.prepare(spec(next_rate));
        assert_eq!(
            first_start(&mut renderer, meta(next, next_rate, revision + 1)),
            carried(residual, (rate, next, next_rate)),
            "cut at {start}->{next}: the incoming source starts at the carried phase"
        );
        renderer.prepared_quantum = None;
    }
}

#[derive(Clone, Copy, Debug)]
enum After {
    StaleWindow,
    Reset,
    Reanchor,
}

#[kithara::test]
#[case::stale_window(After::StaleWindow)]
#[case::reset(After::Reset)]
#[case::reanchor(After::Reanchor)]
fn a_carried_phase_belongs_to_its_revision_until_a_source_reset(#[case] after: After) {
    let mut renderer = renderer(SpeedCurve::Constant(1.25));
    let [(start, rate), (next, next_rate), ..] = CUTS;
    renderer
        .reanchor_source(
            meta(start, rate, 0),
            (
                u128::from(start) * 4 + 1,
                NonZeroU128::new(4).expect("phase"),
            ),
        )
        .expect("fractional entry phase");
    let (law, end, mut emitted) = feed(&mut renderer, meta(start, rate, 0), Some(64));
    emitted.extend(cut(&mut renderer, 1));
    let (starts, residual) = expected(law, end);
    assert_eq!(emitted, starts);
    renderer.prepare(spec(next_rate));
    let incoming = meta(next, next_rate, 1);
    let first = match after {
        After::StaleWindow => {
            assert_eq!(
                first_start(&mut renderer, meta(next, next_rate, 0)),
                (u128::from(next), 1),
                "a window of another revision starts on its own frame"
            );
            carried(residual, (rate, next, next_rate))
        }
        After::Reset => {
            renderer.reset();
            (u128::from(next), 1)
        }
        After::Reanchor => {
            renderer
                .reanchor_source(
                    incoming,
                    (
                        u128::from(next) * 4 + 3,
                        NonZeroU128::new(4).expect("phase"),
                    ),
                )
                .expect("explicit entry");
            (u128::from(next) * 4 + 3, 4)
        }
    };
    assert_eq!(first_start(&mut renderer, incoming), first, "{after:?}");
}

#[kithara::test]
fn a_cut_without_budget_fails_and_one_without_pcm_carries_nothing() {
    let mut renderer = renderer(SpeedCurve::Constant(1.25));
    assert!(matches!(renderer.drain_source_cut(QUANTUM, 1), Ok(None)));
    assert!(renderer.carried_phase.is_none());

    let (start, rate) = CUTS[0];
    feed(&mut renderer, meta(start, rate, 0), Some(64));
    assert!(renderer.drain_source_cut(0, 1).is_err());
    assert!(renderer.carried_phase.is_none());
}
