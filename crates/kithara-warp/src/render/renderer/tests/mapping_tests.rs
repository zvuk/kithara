use std::num::{NonZeroU128, NonZeroUsize};

use kithara_signal::{AudioChunkInfo, ExponentialApproach, FrameCount};
use kithara_sync::SpeedCurve;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::*;
use crate::test_pools::TestPools;

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
    assert_eq!(
        renderer
            .prepare_quantum(meta, 16, 16)
            .expect("an unlinked renderer admits its ordinary source"),
        FrameCount::new(16)
    );
    let span = renderer
        .prepared_quantum
        .expect("ordinary quantum")
        .source_span;
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
            .source_span;
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
    let projection = terminal.source_span;
    assert_eq!(projection.start(), 0);
    assert_eq!(projection.output_frames(), 64);
    assert_eq!(projection.end(), 96);
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
fn an_approach_preserves_one_analytic_basis_between_quanta(#[case] composite: bool) {
    let (mut renderer, meta) = renderer();
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
            .trajectory
            .at_offset(0, meta.spec.sample_rate, 0)
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

#[kithara::test]
fn varispeed_pcm_matches_scalar_geometry_across_spans() {
    let (mut renderer, mut meta) = renderer();
    meta.spec = AudioSpec::new(2, NonZeroU32::new(8_000).expect("fixture sample rate"));
    renderer.prepare(meta.spec);
    let tone = kithara_test_fixtures::unit_fixtures::warp_native_tone();
    let mut resident = residency::SourceResidency::prepare(&renderer.pools, None, 32, 1024, 0, 2)
        .expect("reference source window");
    resident
        .append(
            AudioChunkInfo {
                frames: 1024,
                ..meta
            },
            &tone[..2048],
        )
        .expect("fixture PCM residency");
    renderer.residency = Some(resident);
    renderer.scratch = Some(
        renderer
            .pools
            .get_with_len::<f32>(254)
            .expect("127 stereo output frames"),
    );
    let denominator = NonZeroU128::new(2).expect("fractional phase");
    let spans = [
        kithara_signal::SourceSpan::try_from((129, 1, denominator, meta.spec.sample_rate, 127))
            .expect("half speed with fractional origin"),
        kithara_signal::SourceSpan::try_from((257, 3, denominator, meta.spec.sample_rate, 127))
            .expect("downsampled fractional origin"),
        kithara_signal::SourceSpan::try_from((
            8192,
            64,
            1,
            NonZeroU128::new(128).expect("ramp clock"),
            meta.spec.sample_rate,
            127,
        ))
        .expect("ramp crossing unity speed"),
        kithara_signal::SourceSpan::approaching(
            257,
            NonZeroU128::new(4).expect("approach origin"),
            ExponentialApproach::new(0.5, 1.5, 24.0).expect("approach crossing unity"),
            0,
            meta.spec.sample_rate,
            127,
        )
        .expect("exact approach mapping"),
    ];
    for frames in [0, 1, 7, 127] {
        for original in spans {
            let span = original
                .for_output_range(0..frames)
                .expect("retained output extent");
            let mut expected = vec![0.0; usize::try_from(frames).expect("frame extent") * 2];
            let resident = renderer.residency.as_ref().expect("resident fixture");
            for (frame, output) in expected.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let frame = u64::try_from(frame).expect("scalar frame");
                super::super::super::source_sample::source_frame(
                    resident,
                    super::super::super::source_sample::split_position(
                        span.source_ratio_at(frame)
                            .expect("scalar source coordinate"),
                    )
                    .expect("representable source coordinate"),
                    mapping::span_speed(span, frame).expect("scalar source speed"),
                    None,
                    output,
                )
                .expect("scalar PCM sample");
            }
            renderer
                .render_varispeed(span)
                .expect("mapped varispeed PCM");
            let actual = renderer.scratch.as_deref().expect("rendered PCM");
            assert_eq!(actual.len(), expected.len());
            for (sample, (&actual, expected)) in actual.iter().zip(expected).enumerate() {
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "sample {sample}, frames {frames}, span {span:?}"
                );
            }
        }
    }
}
