use std::num::{NonZeroU32, NonZeroU128};

use kithara_platform::sync::Arc;
use kithara_signal::{AudioChunkInfo, AudioSpec, ExponentialApproach};
use kithara_stretch::StretchKind;
use kithara_test_utils::kithara;

use super::*;
use crate::{
    GridSegment, RegionPlan, Warp, WarpConfig,
    render::renderer::residency::SourceResidency,
    test_pools::{TestPools, pools},
};

fn renderer(regions: bool) -> WarpRenderer<TestPools> {
    let spec = AudioSpec::new(2, NonZeroU32::new(8_000).expect("sample rate"));
    let config = WarpConfig::builder()
        .backend(StretchKind::Signalsmith)
        .keylock(true);
    let config = if regions {
        config
            .region_plan(Arc::new(
                RegionPlan::new(vec![
                    GridSegment::new(0, 100, 1.25),
                    GridSegment::new(100, u64::MAX, 0.75),
                ])
                .expect("two mapped regions"),
            ))
            .build()
    } else {
        config.build()
    };
    let mut renderer = Warp::new((), &config).renderer(spec, pools());
    renderer.prepare_engine_latency(spec).expect("prepared DSP");
    let mut resident =
        SourceResidency::prepare(&renderer.pools, None, 32, 32_768, 0, 2).expect("resident source");
    let tone = kithara_test_fixtures::unit_fixtures::warp_native_tone();
    let samples: Vec<_> = tone.iter().copied().cycle().take(65_536).collect();
    resident
        .append(
            AudioChunkInfo {
                spec,
                frames: 32_768,
                ..Default::default()
            },
            &samples,
        )
        .expect("resident PCM");
    resident
        .remember_mapping(
            SourceSpan::try_from((0, 1, NonZeroU128::MIN, spec.sample_rate, 64))
                .expect("committed source history"),
        )
        .expect("retained history");
    renderer.residency = Some(resident);
    renderer
}

fn spans(rate: NonZeroU32) -> [SourceSpan; 4] {
    [
        SourceSpan::try_from((129, 1, NonZeroU128::new(2).expect("phase"), rate, 127))
            .expect("half speed"),
        SourceSpan::try_from((257, 3, NonZeroU128::new(2).expect("phase"), rate, 127))
            .expect("speed above unity"),
        SourceSpan::try_from((8192, 64, 1, NonZeroU128::new(128).expect("ramp"), rate, 127))
            .expect("ramp crossing unity"),
        SourceSpan::approaching(
            257,
            NonZeroU128::new(4).expect("approach phase"),
            ExponentialApproach::new(0.5, 1.5, 24.0).expect("approach across unity"),
            0,
            rate,
            127,
        )
        .expect("approach span"),
    ]
}

fn scalar_positions(
    renderer: &WarpRenderer<TestPools>,
    span: SourceSpan,
    offset: i64,
    nominal: Option<SourceSpan>,
    frame: u64,
) -> Result<(Option<Fraction>, Option<Fraction>), ElasticError> {
    let resident = renderer
        .residency
        .as_ref()
        .ok_or(ElasticError::PoolCapacity)?;
    let position = match nominal {
        None => resident.history_position(offset.unsigned_abs()),
        Some(nominal) => Some(
            renderer.map_nominal_position(
                span.start(),
                nominal
                    .source_ratio_at(frame)
                    .ok_or(ElasticError::SampleCountOverflow)?,
            )?,
        ),
    };
    let next = nominal
        .map(|nominal| {
            renderer.map_nominal_position(
                span.start(),
                nominal
                    .source_ratio_at(frame + 1)
                    .ok_or(ElasticError::SampleCountOverflow)?,
            )
        })
        .transpose()?;
    let fraction = |(numerator, denominator)| Fraction {
        numerator,
        denominator,
    };
    Ok((position.map(fraction), next.map(fraction)))
}

#[kithara::test]
fn projected_source_pcm_matches_scalar_geometry_and_history() {
    for regions in [false, true] {
        let mut renderer = renderer(regions);
        let span = spans(renderer.spec.sample_rate)[0];
        for terminal in [None, Some(90)] {
            renderer.terminal_source_end = terminal;
            for nominal in spans(renderer.spec.sample_rate)
                .map(Some)
                .into_iter()
                .chain([None])
            {
                for frames in [0, 1, 7, 127] {
                    for offset in [-65_i64, -7, 0, i64::MAX] {
                        renderer
                            .prepare_projection_buffer(frames)
                            .expect("output capacity");
                        renderer.pending_source.as_mut().expect("output").fill(0.0);
                        let mut expected = renderer
                            .pending_source
                            .as_deref()
                            .expect("retained output buffer")
                            .to_vec();
                        let scalar = (|| {
                            for (frame, output) in expected
                                .as_chunks_mut::<2>()
                                .0
                                .iter_mut()
                                .take(frames)
                                .enumerate()
                            {
                                let frame_offset = i64::try_from(frame)
                                    .map_err(|_| ElasticError::SampleCountOverflow)?;
                                let (position, next) = scalar_positions(
                                    &renderer,
                                    span,
                                    offset
                                        .checked_add(frame_offset)
                                        .ok_or(ElasticError::SampleCountOverflow)?,
                                    nominal,
                                    u64::try_from(frame)
                                        .map_err(|_| ElasticError::SampleCountOverflow)?,
                                )?;
                                sample_projected_frame(
                                    renderer.residency.as_ref().expect("resident source"),
                                    position,
                                    next,
                                    span,
                                    terminal,
                                    output,
                                )?;
                            }
                            Ok::<_, ElasticError>(())
                        })();
                        let actual =
                            renderer.project_source_segment(span, offset, 0..frames, nominal);
                        assert_eq!(
                            actual, scalar,
                            "regions={regions}, offset={offset}, frames={frames}"
                        );
                        let actual = renderer.pending_source.as_deref().expect("projected PCM");
                        assert_eq!(actual.len(), expected.len());
                        for (actual, expected) in actual.iter().zip(expected) {
                            assert_eq!(actual.to_bits(), expected.to_bits());
                        }
                    }
                }
            }
        }
    }
}

fn scalar_projected(
    renderer: &mut WarpRenderer<TestPools>,
    span: SourceSpan,
) -> Result<(), ElasticError> {
    let capabilities = renderer
        .engine
        .as_ref()
        .ok_or(ElasticError::PoolCapacity)?
        .capabilities();
    assert_eq!(
        renderer.native_projection_speed(capabilities, renderer.current_keylock)?,
        None
    );
    let channels = usize::from(renderer.spec.channels);
    let stages = renderer.projection_stages()?;
    let lookahead = (capabilities.latency().first() + capabilities.latency().second()) * stages;
    if !renderer.active {
        renderer.prime_projected(span, stages)?;
    }
    let frames = renderer.prepare_projected_output(span, lookahead, channels)?;
    for frame in 0..frames {
        let speed = span_speed(
            span,
            u64::try_from(frame).map_err(|_| ElasticError::SampleCountOverflow)?,
        )?;
        let factors = pitch_factors(speed, stages);
        renderer.apply_pitch(factors[0])?;
        let input = renderer
            .pending_source
            .as_deref()
            .ok_or(ElasticError::PoolCapacity)?;
        let input = &input[frame * channels..(frame + 1) * channels];
        let scratch = renderer
            .scratch
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        let output = &mut scratch[frame * channels..(frame + 1) * channels];
        process_projected_frame(
            renderer
                .engine
                .as_mut()
                .ok_or(ElasticError::PoolCapacity)?
                .as_mut(),
            renderer.projection.as_mut(),
            factors,
            stages,
            input,
            output,
            channels,
        )?;
    }
    renderer.clear_pending_source();
    renderer.active = true;
    Ok(())
}

#[kithara::test]
fn projected_dsp_matches_scalar_speeds_across_successive_spans() {
    let mut actual = renderer(true);
    let mut scalar = renderer(true);
    for frames in [1, 7, 127] {
        for span in spans(actual.spec.sample_rate) {
            let span = span.for_output_range(0..frames).expect("output extent");
            scalar_projected(&mut scalar, span).expect("scalar DSP");
            actual.render_projected(span).expect("projected DSP");
            let actual = actual.scratch.as_deref().expect("actual PCM");
            let expected = scalar.scratch.as_deref().expect("scalar PCM");
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(actual.to_bits(), expected.to_bits(), "span={span:?}");
            }
        }
    }
}

#[kithara::test]
fn affine_speed_keeps_scalar_bits_at_full_width_boundaries() {
    let rate = NonZeroU32::new(44_100).expect("sample rate");
    for (origin, step, denominator, frames) in [
        (0, 0, 1, 17),
        (129, 1, 2, 257),
        (257, 3, 2, 257),
        (u128::MAX / 4, u128::MAX / 1024, u128::MAX / 2, 257),
        (u128::from(u64::MAX) * 1024 - 257, 1, 1024, 257),
        (0, 1, u128::MAX, u64::MAX),
    ] {
        let span = SourceSpan::try_from((
            origin,
            step,
            NonZeroU128::new(denominator).expect("nonzero denominator"),
            rate,
            frames,
        ))
        .expect("validated affine source extent");
        assert!(span.is_affine());
        let speed = span_speed(span, 0).expect("first scalar speed");
        for frame in [0, 1, frames / 2, frames - 1] {
            let scalar = span_speed(span, frame).expect("scalar speed inside the extent");
            assert_eq!(speed.to_bits(), scalar.to_bits(), "{span:?}, {frame}");
        }
        assert_eq!(
            span_speed(span, frames),
            Err(ElasticError::SampleCountOverflow)
        );
    }
}
