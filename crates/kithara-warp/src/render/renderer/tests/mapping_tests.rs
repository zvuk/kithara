use std::num::{NonZeroU128, NonZeroUsize};

use kithara_platform::sync::Arc;
use kithara_signal::{AudioChunkInfo, ExponentialApproach, FrameCount};
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::*;
use crate::{GridSegment, RegionPlan, test_pools::TestPools};

fn renderer() -> (WarpRenderer<TestPools>, AudioChunkInfo) {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
    let config = WarpConfig::builder()
        .render_quantum_frames(NonZeroUsize::new(128).expect("quantum"))
        .build();
    (
        crate::Warp::new((), &config).renderer(spec, crate::test_pools::pools()),
        AudioChunkInfo {
            spec,
            ..Default::default()
        },
    )
}

#[kithara::test]
fn a_renderer_without_an_entered_plan_names_no_entry_source() {
    let (mut renderer, meta) = renderer();
    renderer.prepare(meta.spec);
    assert!(renderer.plan.is_none());
    assert_eq!(
        renderer
            .prepare_quantum(meta, 16, 16)
            .expect("an unlinked renderer admits its ordinary source"),
        FrameCount::new(16)
    );
    let span = renderer
        .prepared_quantum
        .expect("ordinary quantum")
        .source_span
        .expect("ordinary source mapping");
    assert_eq!(span.start(), 0);
    assert_eq!(span.end(), 16);
    let nonlinear = kithara_signal::SourceSpan::approaching(
        0,
        NonZeroU128::MIN,
        ExponentialApproach::new(1.0, 0.9, 220.5).expect("nonlinear input law"),
        0,
        meta.spec.sample_rate,
        16,
    )
    .expect("valid nonlinear input map");
    assert!(matches!(
        renderer.prepare_source_window(
            AudioChunkInfo {
                source_span: Some(nonlinear),
                ..meta
            },
            16,
            16
        ),
        Err(crate::WarpRenderError::UnsupportedSourceGeometry)
    ));
    #[cfg(feature = "stretch-signalsmith")]
    for (pcm_rate, source_rate, first, origin, step, denominator, requested_ns, expected) in [
        (1_000, 1_000, 37, 151, 4, 4, 38_300_000, 0.55_f64),
        (1_000, 1_000, 36, 147, 4, 4, 37_300_000, 0.55),
        (2_000, 1_000, 500, 200, 3, 2, 101_250_000, 5.0 / 6.0),
        (
            48_000,
            44_100,
            48_000,
            2_116_800_000,
            44_100,
            48_000,
            1_000_011_458,
            0.549984,
        ),
    ] {
        let rate = NonZeroU32::new(pcm_rate).expect("actual PCM rate");
        let spec = AudioSpec::new(2, rate);
        let basis = kithara_signal::SourceSpan::try_from((
            origin,
            step,
            NonZeroU128::new(denominator).expect("native ratio"),
            NonZeroU32::new(source_rate).expect("raw clock"),
            4096,
        ))
        .expect("actual affine input");
        let mut renderer = crate::Warp::new(
            (),
            &WarpConfig::builder()
                .backend(StretchKind::Signalsmith)
                .build(),
        )
        .renderer(spec, crate::test_pools::pools());
        renderer.prepare(spec);
        let meta = AudioChunkInfo {
            spec,
            frame_offset: first,
            frames: 4096,
            source_span: Some(basis),
            ..Default::default()
        };
        let position = basis
            .output_ratio_at_source(
                first,
                (
                    u128::from(requested_ns) * u128::from(source_rate),
                    NonZeroU128::new(1_000_000_000).expect("raw nanosecond clock"),
                ),
            )
            .expect("exact physical phase");
        renderer
            .reanchor_source(meta, position)
            .expect("physical reanchor");
        let frames = renderer
            .prepare_source_window(meta, 4096, 1)
            .expect("real PCM prefix")
            .get();
        let physical = renderer
            .prepared_quantum
            .expect("prepared physical quantum")
            .source_span
            .expect("physical mapping");
        let samples = (0..frames)
            .flat_map(|offset| {
                let sample = offset.to_f32().expect("physical PCM index");
                [sample, -sample]
            })
            .collect::<Vec<_>>();
        let input = AudioChunk::new(
            AudioChunkInfo {
                frames: u32::try_from(frames).expect("input extent"),
                source_span: Some(
                    basis
                        .for_output_range(0..u64::try_from(frames).expect("input extent"))
                        .expect("actual PCM slice"),
                ),
                ..meta
            },
            crate::test_pools::sample_buffer(&renderer.pools, &samples),
        );
        let output = renderer
            .render_quantum(input)
            .continue_value()
            .expect("prepared input")
            .expect("real output");
        assert_eq!(
            output.samples[0],
            expected.to_f32().expect("expected interpolated PCM")
        );
        assert_eq!(
            output.samples[1],
            -expected.to_f32().expect("expected interpolated PCM")
        );
        let published = output.meta.source_span.expect("raw publication");
        assert_eq!(published.sample_rate(), rate);
        assert_eq!(
            published.position_at(0),
            Some(kithara_platform::time::Duration::from_nanos(requested_ns))
        );
        assert_eq!(
            renderer
                .trajectory
                .span(first, rate, 1)
                .expect("physical continuation")
                .source_ratio_at(0),
            physical.source_ratio_at(1)
        );
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn a_short_decoder_suffix_requests_the_next_mapped_source_frame() {
    let (mut renderer, meta) = renderer();
    renderer
        .set_speed(SpeedCurve::Constant(2.0), 1)
        .expect("mapped speed");
    renderer.prepare(meta.spec);
    let requested = renderer
        .prepare_quantum(meta, 1, 1)
        .expect("one mapped output frame spans decoder chunks");
    assert_eq!(requested.get(), 2);
    let prepared = renderer.prepared_quantum.expect("prepared request");
    assert_eq!(prepared.active_frames, 2);
    assert_eq!(prepared.frames, 17);
    assert_eq!(
        renderer
            .prepare_source_window(meta, 1, 1)
            .expect("physical lookahead reuses the selected quantum"),
        FrameCount::new(17)
    );
    assert_eq!(
        renderer
            .prepared_quantum
            .expect("prepared request")
            .source_span
            .expect("projected request")
            .output_frames(),
        1
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn finite_projection_clips_a_quantum_before_resolving_past_the_recording() {
    let (mut renderer, mut meta) = renderer();
    renderer.plan = Some(Arc::new(
        RegionPlan::new(vec![GridSegment::new(0, 8_820_000, 1.0)])
            .expect("the finite recording is one region"),
    ));
    renderer.prepare(meta.spec);
    let start = 8_819_968;
    meta.frame_offset = start;
    let requested = renderer
        .prepare_quantum(meta, 32, usize::MAX)
        .expect("a covered final quantum survives an out-of-domain search midpoint");
    assert_eq!(requested.get(), 32);
    let prepared = renderer.prepared_quantum.expect("prepared quantum");
    let projection = prepared.source_span.expect("projected quantum");
    assert_eq!(projection.output_frames(), 32);
    assert_eq!(projection.end(), 8_820_000);
    assert_eq!(prepared.landing_frames, Some(32));
    assert_eq!(prepared.source_start, start);
    assert_eq!(
        projection
            .source_ratio_at(32)
            .map(|(numerator, denominator)| (numerator, denominator.get())),
        Some((8_820_000, 1))
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
#[kithara::hang_watchdog]
fn terminal_projection_recomputes_both_endpoints_and_keeps_the_selected_rate() {
    hang_tick!();
    let (mut renderer, meta) = renderer();
    hang_reset!();
    renderer
        .set_speed(SpeedCurve::Constant(1.5), 1)
        .expect("mapped speed");
    renderer.prepare(meta.spec);
    hang_tick!();
    let requested = renderer
        .prepare_quantum(meta, 4096, usize::MAX)
        .expect("initial quantum resolves");
    hang_reset!();
    let prepared = renderer.prepared_quantum.expect("prepared quantum");
    let rate = prepared.rate;
    assert_eq!(requested.get(), 192);
    assert_eq!(prepared.active_frames, 192);
    assert_eq!(prepared.frames, 208);
    assert_eq!(
        renderer.prepare_terminal_quantum(96),
        Some(FrameCount::new(96))
    );
    let terminal = renderer
        .prepared_quantum
        .expect("terminal quantum is retained");
    assert_eq!(terminal.rate.revision(), rate.revision());
    assert_eq!(terminal.rate.speed(), rate.speed());
    assert_eq!(terminal.speed, 1.5);
    assert_eq!(terminal.active_frames, 96);
    assert_eq!(terminal.source_start, 0);
    let projection = terminal
        .source_span
        .expect("terminal projection is retained");
    assert_eq!(projection.start(), 0);
    assert_eq!(projection.output_frames(), 64);
    assert_eq!(projection.end(), 96);
    assert_eq!(terminal.landing_frames, Some(64));
    assert_eq!(
        projection
            .source_ratio_at(64)
            .map(|(numerator, denominator)| (numerator, denominator.get())),
        Some((96, 1))
    );
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
#[case::single(false)]
#[case::finite_segments(true)]
fn an_approach_region_preserves_one_analytic_basis_between_quanta(#[case] composite: bool) {
    let (mut renderer, meta) = renderer();
    renderer.plan = Some(Arc::new(
        RegionPlan::new(vec![GridSegment::new(0, 10_000, 1.25)]).expect("uniform region"),
    ));
    let curve = ExponentialApproach::new(1.0, 0.9, 220.5).expect("approach");
    let command = if composite {
        SpeedCurve::ApproachSteps(vec![(0, curve)].into())
    } else {
        SpeedCurve::Approach(curve)
    };
    renderer.set_speed(command, 1).expect("accepted approach");
    renderer.prepare(meta.spec);
    let whole = renderer
        .mapping_span(0, meta.spec.sample_rate, 256)
        .expect("whole mapping");
    let first = renderer
        .mapping_span(0, meta.spec.sample_rate, 128)
        .expect("first quantum");
    renderer
        .trajectory
        .advance(first)
        .expect("first quantum consumed");
    let second = renderer
        .mapping_span(0, meta.spec.sample_rate, 128)
        .expect("second quantum");
    assert_eq!(
        renderer
            .mapped_position(0, meta.spec.sample_rate, 0)
            .expect("current mapped position"),
        first.source_ratio_at(128).expect("consumed mapped point")
    );
    assert_eq!(first.followed_by(second), Some(whole));
    for frame in 0..=128 {
        assert_eq!(first.source_ratio_at(frame), whole.source_ratio_at(frame));
        assert_eq!(
            second.source_ratio_at(frame),
            whole.source_ratio_at(128 + frame)
        );
    }
}

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
#[kithara::test]
fn an_approach_crosses_a_region_boundary_with_its_actual_source_origin() {
    use crate::render::trajectory::Fraction;

    let (mut renderer, meta) = renderer();
    renderer.plan = Some(Arc::new(
        RegionPlan::new(vec![
            GridSegment::new(0, 100, 1.25),
            GridSegment::new(100, 10_000, 0.75),
        ])
        .expect("two source regions"),
    ));
    renderer
        .set_speed(
            SpeedCurve::Approach(ExponentialApproach::new(1.0, 0.9, 220.5).expect("approach")),
            1,
        )
        .expect("accepted approach");
    renderer.prepare(meta.spec);
    let nominal = renderer
        .trajectory
        .span(0, meta.spec.sample_rate, 512)
        .expect("nominal law");
    let boundary = Fraction::new(125, 1).expect("nominal boundary");
    let mut cursor = 0;
    while cursor < 512 {
        let span = renderer
            .mapping_span(0, meta.spec.sample_rate, 64.min(512 - cursor))
            .expect("mapped region quantum");
        for offset in 0..=span.output_frames() {
            let frame = u64::try_from(cursor).expect("cursor") + offset;
            let (numerator, denominator) = nominal.source_ratio_at(frame).expect("nominal point");
            let source = Fraction {
                numerator,
                denominator,
            };
            let expected = if numerator <= 125 * denominator.get() {
                source.mul(Fraction::new(4, 5).expect("first inverse ratio"))
            } else {
                source
                    .sub(boundary)
                    .and_then(|advance| advance.mul(Fraction::new(4, 3)?))
                    .and_then(|advance| advance.add(Fraction::new(100, 1)?))
            }
            .expect("independent region projection");
            assert_eq!(
                span.source_ratio_at(offset),
                Some((expected.numerator, expected.denominator))
            );
        }
        cursor += usize::try_from(span.output_frames()).expect("quantum length");
        renderer
            .trajectory
            .advance(span)
            .expect("mapped quantum consumed");
    }
}
