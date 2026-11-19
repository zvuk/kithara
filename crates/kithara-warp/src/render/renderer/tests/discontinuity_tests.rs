use std::num::{NonZeroU128, NonZeroUsize};

use kithara_signal::{AudioChunkInfo, ExponentialApproach, SourceSpan};
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;

use super::*;
use crate::{render::trajectory::Fraction, test_pools::TestPools};

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

fn prepared(renderer: &mut WarpRenderer<TestPools>, meta: AudioChunkInfo) -> SourceSpan {
    renderer
        .prepare_source_window(meta, 65_536, QUANTUM)
        .expect("source window");
    renderer
        .prepared_quantum
        .expect("prepared quantum")
        .source_span
}

/// Whole-interval error of the incoming first sample against the continuous law.
fn lead(continuous: SourceSpan, count: u64, actual: Fraction, expected: Fraction) -> f64 {
    let step = fraction(
        continuous
            .source_ratio_at(count + 1)
            .expect("next boundary"),
    )
    .sub(fraction(
        continuous.source_ratio_at(count).expect("boundary"),
    ))
    .and_then(Fraction::as_f64)
    .expect("output interval");
    let (actual, expected) = (
        actual.as_f64().expect("actual"),
        expected.as_f64().expect("expected"),
    );
    (expected - actual) / step
}

#[kithara::test]
fn a_seek_after_a_clock_change_starts_on_its_own_frame() {
    let config = WarpConfig::builder()
        .render_quantum_frames(NonZeroUsize::new(QUANTUM).expect("quantum"))
        .build();
    let [(start, rate), (next, next_rate), ..] = CUTS;
    let mut renderer =
        crate::Warp::new((), &config).renderer(spec(rate), crate::test_pools::pools());
    renderer.prepare(spec(rate));
    let meta = AudioChunkInfo {
        spec: spec(rate),
        frame_offset: start,
        ..Default::default()
    };
    renderer
        .reanchor_source(
            meta,
            (
                u128::from(start) * 4 + 1,
                NonZeroU128::new(4).expect("phase"),
            ),
        )
        .expect("fractional entry phase");
    prepared(&mut renderer, meta);
    renderer
        .prepare_discontinuity_quantum(64)
        .expect("the outgoing tail prepares");
    renderer.prepared_quantum = None;
    renderer.reset();
    renderer.reset();
    renderer.prepare(spec(next_rate));
    let incoming = prepared(
        &mut renderer,
        AudioChunkInfo {
            spec: spec(next_rate),
            frame_offset: next,
            ..Default::default()
        },
    );
    let first = fraction(incoming.source_ratio_at(0).expect("incoming start"));
    assert_eq!(
        (first.numerator, first.denominator.get()),
        (u128::from(next), 1)
    );
}

#[kithara::test]
#[case::unity_below_lattice(SpeedCurve::Constant(1.0), (24, 25), 64)]
#[case::unity_above_lattice(SpeedCurve::Constant(1.0), (1, 4), 64)]
#[case::slower_below_lattice(SpeedCurve::Constant(0.75), (24, 25), 64)]
#[case::faster_above_lattice(SpeedCurve::Constant(1.25), (1, 4), 64)]
#[case::approach(
    SpeedCurve::Approach(ExponentialApproach::new(1.0, 0.9, 220.5).expect("approach")),
    (1, 4),
    64
)]
#[case::staged_below_one_interval(SpeedCurve::Constant(1.0), (5, 4), 1)]
fn a_source_clock_change_carries_the_tail_phase_into_the_incoming_source(
    #[case] curve: SpeedCurve,
    #[case] phase: (u128, u128),
    #[case] staged: usize,
) {
    let config = WarpConfig::builder()
        .render_quantum_frames(NonZeroUsize::new(QUANTUM).expect("quantum"))
        .build();
    let (start, rate) = CUTS[0];
    let mut renderer =
        crate::Warp::new((), &config).renderer(spec(rate), crate::test_pools::pools());
    renderer.set_speed(curve, 1).expect("speed law");
    renderer.prepare(spec(rate));
    let meta = AudioChunkInfo {
        spec: spec(rate),
        frame_offset: start,
        ..Default::default()
    };
    let denominator = NonZeroU128::new(phase.1).expect("phase denominator");
    renderer
        .reanchor_source(meta, (u128::from(start) * phase.1 + phase.0, denominator))
        .expect("fractional entry phase");
    let mut continuous = prepared(&mut renderer, meta);

    for pair in CUTS.windows(2) {
        let [(start, rate), (next, next_rate)] = [pair[0], pair[1]];
        let end = start + u64::try_from(staged).expect("staged frames");
        let expected_count = (0..continuous.output_frames())
            .take_while(|&frame| {
                let (numerator, denominator) = continuous
                    .source_ratio_at(frame)
                    .expect("source coordinate");
                numerator / denominator.get() < u128::from(end)
            })
            .count();
        renderer
            .prepare_discontinuity_quantum(staged)
            .expect("the outgoing tail prepares without a render failure");
        let tail = renderer
            .prepared_quantum
            .take()
            .expect("outgoing tail")
            .source_span;
        let count = tail.output_frames();
        assert_eq!(
            usize::try_from(count).expect("tail count"),
            expected_count,
            "cut at {start}: the tail keeps every sample that starts before source end {end}"
        );
        renderer.reset();
        renderer.prepare(spec(next_rate));
        let incoming = prepared(
            &mut renderer,
            AudioChunkInfo {
                spec: spec(next_rate),
                frame_offset: next,
                ..Default::default()
            },
        );
        let carried = fraction(continuous.source_ratio_at(count).expect("tail boundary"))
            .sub(Fraction::new(u128::from(end), 1).expect("outgoing end"))
            .expect("the tail keeps every sample before its source end");
        let expected = Fraction::new(
            carried.numerator * u128::from(next_rate),
            carried.denominator.get() * u128::from(rate),
        )
        .and_then(|carried| carried.add(Fraction::new(u128::from(next), 1)?))
        .expect("incoming coordinate");
        let actual = fraction(incoming.source_ratio_at(0).expect("incoming start"));
        assert_eq!(
            (
                usize::try_from(count).expect("tail count"),
                (actual.numerator, actual.denominator)
            ),
            (expected_count, (expected.numerator, expected.denominator)),
            "cut at {start}->{next}: incoming leads the continuous law by {} output intervals",
            lead(continuous, count, actual, expected),
        );
        continuous = incoming;
    }
}
