use std::num::NonZero;

use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, PcmSpec};
use kithara_platform::sync::Arc;
use kithara_stretch::StretchKind;
use kithara_test_utils::kithara;

use super::{
    super::*,
    common::{Consts, chunk, chunk_at, keylocked, processor, sine, spec},
};
use crate::region::{GridSegment, RegionPlan};

fn render_step(stage: &mut TimeStretchProcessor) -> TempoStep {
    let mut output = vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
    let credit = OutputCredit::new(
        &mut output,
        usize::from(Consts::CH),
        TimeStretchProcessor::PRESENTATION_FRAMES,
    );
    TempoStage::render(stage, None, credit, &mut |_| {}).expect("tempo source renders")
}

fn drain_discontinuity(stage: &mut TimeStretchProcessor) {
    let mut debt =
        TempoStage::begin_discontinuity(stage).expect("tempo discontinuity begins after admission");
    loop {
        let mut output =
            vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
        let credit = OutputCredit::new(
            &mut output,
            usize::from(Consts::CH),
            TimeStretchProcessor::PRESENTATION_FRAMES,
        );
        if matches!(
            TempoStage::render_discontinuity(stage, &mut debt, credit, &mut |_| {})
                .expect("tempo discontinuity drains"),
            TempoDiscontinuityStep::Drained
        ) {
            break;
        }
    }
}

#[kithara::test]
fn control_boundary_is_withheld_while_source_is_admitted() {
    let controls = StretchControls::new(1.0);
    let mut stage = processor(Arc::clone(&controls));
    let source = sine(TimeStretchProcessor::PRESENTATION_FRAMES);
    TempoStage::push_source(&mut stage, chunk(&source)).expect("source quantum is admitted");

    controls.set_speed(0.5);
    TempoStage::service_off_rt(&mut stage, TempoPrepareRequest::Current { spec: spec() })
        .expect("control replacement prepares off-RT");
    assert!(stage.prepared.is_some());
    assert_eq!(TempoStage::prepared_boundary(&stage), None);

    assert!(matches!(
        render_step(&mut stage),
        TempoStep::Rendered { .. }
    ));
    assert_eq!(TempoStage::buffered_source_quanta(&stage), 0);
    assert!(TempoStage::prepared_boundary(&stage).is_some());
}

#[kithara::test]
fn decoder_boundary_is_withheld_while_source_is_admitted() {
    let controls = StretchControls::new(1.0);
    let mut stage = processor(controls);
    let source = sine(TimeStretchProcessor::PRESENTATION_FRAMES);
    TempoStage::push_source(&mut stage, chunk(&source)).expect("source quantum is admitted");
    let replacement = PcmSpec::new(
        Consts::CH,
        NonZero::new(48_000).expect("replacement rate is non-zero"),
    );

    TempoStage::service_off_rt(
        &mut stage,
        TempoPrepareRequest::DecoderBoundary { spec: replacement },
    )
    .expect("decoder replacement prepares off-RT");
    assert!(matches!(
        stage.prepared.as_ref().map(|prepared| prepared.cause),
        Some(PrepareCause::DecoderBoundary)
    ));
    assert_eq!(TempoStage::prepared_boundary(&stage), None);

    assert!(matches!(
        render_step(&mut stage),
        TempoStep::Rendered { .. }
    ));
    assert_eq!(TempoStage::buffered_source_quanta(&stage), 0);
    assert!(TempoStage::prepared_boundary(&stage).is_some());
}

#[kithara::test]
fn region_boundary_preempts_and_defers_queued_decoder_boundary() {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(StretchKind::default());
    let plan = RegionPlan::new(vec![
        GridSegment::new(0, 256, 0.9),
        GridSegment::new(256, 512, 1.1),
    ])
    .expect("fixture region plan is valid");
    controls.set_region_plan(Some(Arc::new(plan)));
    let mut stage = processor(controls);
    let source = sine(TimeStretchProcessor::PRESENTATION_FRAMES);
    TempoStage::push_source(&mut stage, chunk(&source)).expect("source quantum is admitted");

    let reached_boundary = (0..64).any(|_| matches!(render_step(&mut stage), TempoStep::Preparing));
    assert!(
        reached_boundary,
        "fixture must retain source at a region boundary"
    );
    assert_eq!(TempoStage::buffered_source_quanta(&stage), 1);

    let replacement = PcmSpec::new(
        Consts::CH,
        NonZero::new(48_000).expect("replacement rate is non-zero"),
    );
    TempoStage::service_off_rt(
        &mut stage,
        TempoPrepareRequest::DecoderBoundary { spec: replacement },
    )
    .expect("region replacement prepares before the decoder replacement");
    let region = TempoStage::prepared_boundary(&stage).expect("region boundary is published");
    assert!(matches!(
        stage.prepared.as_ref().map(|prepared| prepared.cause),
        Some(PrepareCause::RegionBoundary)
    ));
    assert_eq!(
        stage.prepared.as_ref().map(|prepared| prepared.core.spec),
        Some(spec())
    );

    drain_discontinuity(&mut stage);
    TempoStage::commit_prepared(&mut stage, region).expect("region replacement commits first");
    TempoStage::service_off_rt(
        &mut stage,
        TempoPrepareRequest::DecoderBoundary { spec: replacement },
    )
    .expect("queued decoder replacement is reissued on the next cycle");
    assert!(matches!(
        stage.prepared.as_ref().map(|prepared| prepared.cause),
        Some(PrepareCause::DecoderBoundary)
    ));
    assert_eq!(
        stage.prepared.as_ref().map(|prepared| prepared.core.spec),
        Some(replacement)
    );
    assert_eq!(TempoStage::prepared_boundary(&stage), None);

    while TempoStage::buffered_source_quanta(&stage) != 0 {
        assert!(!matches!(render_step(&mut stage), TempoStep::Preparing));
    }
    let decoder = TempoStage::prepared_boundary(&stage).expect("decoder boundary follows region");
    assert_ne!(decoder, region);
}

#[kithara::test]
fn repeated_off_rt_service_preserves_the_exact_decoder_boundary() {
    let controls = StretchControls::new(0.5);
    controls.set_keylock(true);
    controls.set_backend(StretchKind::default());
    let mut stage = processor(Arc::clone(&controls));

    TempoStage::service_off_rt(
        &mut stage,
        TempoPrepareRequest::DecoderBoundary { spec: spec() },
    )
    .expect("decoder replacement prepares off-RT");
    let first = TempoStage::prepared_boundary(&stage).expect("boundary is published");

    TempoStage::service_off_rt(
        &mut stage,
        TempoPrepareRequest::DecoderBoundary { spec: spec() },
    )
    .expect("identical service request is idempotent");
    let second = TempoStage::prepared_boundary(&stage).expect("boundary stays published");

    assert_eq!(first, second);
}

#[kithara::test]
fn off_rt_release_allows_seek_after_a_committed_tempo_boundary() {
    let controls = StretchControls::new(1.0);
    let mut stage = processor(Arc::clone(&controls));
    controls.set_speed(0.5);
    TempoStage::service_off_rt(&mut stage, TempoPrepareRequest::Current { spec: spec() })
        .expect("replacement core prepares off-RT");
    let boundary = TempoStage::prepared_boundary(&stage).expect("tempo boundary is published");
    let mut debt = TempoStage::begin_discontinuity(&mut stage)
        .expect("idle core creates a finite discontinuity");
    let mut output = vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
    let credit = OutputCredit::new(
        &mut output,
        usize::from(Consts::CH),
        TimeStretchProcessor::PRESENTATION_FRAMES,
    );
    assert!(matches!(
        TempoStage::render_discontinuity(&mut stage, &mut debt, credit, &mut |_| {})
            .expect("idle discontinuity drains"),
        TempoDiscontinuityStep::Drained
    ));
    TempoStage::commit_prepared(&mut stage, boundary).expect("replacement core commits on RT");

    assert!(
        TempoStage::deactivate(&mut stage, &mut |_| {}).is_err(),
        "RT deactivation must not overwrite an unreleased retired core"
    );
    TempoStage::release_retired_off_rt(&mut stage);
    TempoStage::deactivate(&mut stage, &mut |_| {})
        .expect("off-RT release makes the following seek deactivation safe");
}

#[kithara::test]
fn missing_initial_core_waits_without_consuming_admitted_source() {
    let controls = StretchControls::new(0.5);
    let mut stage = TimeStretchProcessor::new(controls, spec(), PcmPool::default());
    let source = sine(TimeStretchProcessor::PRESENTATION_FRAMES);
    TempoStage::push_source(&mut stage, chunk(&source)).expect("source admission is retained");
    let mut output = vec![0.0; source.len()];
    let credit = OutputCredit::new(
        &mut output,
        usize::from(Consts::CH),
        TimeStretchProcessor::PRESENTATION_FRAMES,
    );

    assert!(matches!(
        TempoStage::render(&mut stage, None, credit, &mut |_| {})
            .expect("missing preparation is a typed wait"),
        TempoStep::Preparing
    ));
    assert_eq!(TempoStage::buffered_source_quanta(&stage), 1);
}

#[kithara::test]
fn invalid_speed_is_rejected_during_off_rt_preparation() {
    let controls = StretchControls::new(f32::NAN);
    let mut stage = TimeStretchProcessor::new(controls, spec(), PcmPool::default());

    assert!(matches!(
        TempoStage::service_off_rt(&mut stage, TempoPrepareRequest::Current { spec: spec() }),
        Err(DecodeError::InvalidData {
            detail: "tempo stage cannot adopt an invalid playback speed"
        })
    ));
    assert!(stage.active.is_none());
}

#[cfg(feature = "stretch-signalsmith")]
#[kithara::test]
fn signalsmith_discontinuity_tail_advances_the_source_frontier_exactly() {
    let mut stage = keylocked(StretchKind::Signalsmith, 0.5);
    let source = sine(TimeStretchProcessor::PRESENTATION_FRAMES);
    TempoStage::push_source(&mut stage, chunk(&source)).expect("one source quantum is admitted");
    while TempoStage::buffered_source_quanta(&stage) != 0 {
        let mut output =
            vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
        let credit = OutputCredit::new(
            &mut output,
            usize::from(Consts::CH),
            TimeStretchProcessor::PRESENTATION_FRAMES,
        );
        TempoStage::render(&mut stage, None, credit, &mut |_| {})
            .expect("source quantum renders before the barrier");
    }
    let held = TempoStage::held_source_frames(&stage);
    assert!(held > 0);
    let expected_tail = stage
        .active
        .as_ref()
        .and_then(|core| core.backend.as_ref())
        .expect("fixture has an active Signalsmith backend")
        .max_tail_samples()
        / usize::from(Consts::CH);
    assert!(
        expected_tail > TimeStretchProcessor::PRESENTATION_FRAMES,
        "fixture must exercise a multi-block drain tail"
    );
    let mut debt = TempoStage::begin_discontinuity(&mut stage)
        .expect("ordered discontinuity creates finite debt");
    let mut rendered_tail = 0;
    let admitted_end = u64::try_from(TimeStretchProcessor::PRESENTATION_FRAMES).unwrap();
    let mut previous_frontier = admitted_end - held;
    loop {
        let mut output =
            vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
        let credit = OutputCredit::new(
            &mut output,
            usize::from(Consts::CH),
            TimeStretchProcessor::PRESENTATION_FRAMES,
        );
        match TempoStage::render_discontinuity(&mut stage, &mut debt, credit, &mut |_| {})
            .expect("discontinuity debt drains")
        {
            TempoDiscontinuityStep::Drained => {
                assert_eq!(TempoStage::held_source_frames(&stage), 0);
                break;
            }
            TempoDiscontinuityStep::Rendered { frames, .. } => {
                assert!(frames <= TimeStretchProcessor::PRESENTATION_FRAMES);
                rendered_tail += frames;
                let released = u128::from(held) * u128::try_from(rendered_tail).unwrap()
                    / u128::try_from(expected_tail).unwrap();
                let expected_held = held - u64::try_from(released).unwrap();
                let current_held = TempoStage::held_source_frames(&stage);
                assert_eq!(current_held, expected_held);
                let frontier = admitted_end - current_held;
                assert!(
                    frontier >= previous_frontier,
                    "source frontier regressed from {previous_frontier} to {frontier}"
                );
                previous_frontier = frontier;
            }
        }
    }

    assert_eq!(rendered_tail, expected_tail);
    assert_eq!(previous_frontier, admitted_end);
}

#[cfg(feature = "stretch-signalsmith")]
#[kithara::test]
fn signalsmith_reconfigure_after_discontinuity_adopts_the_replacement_spec_and_live_controls() {
    let controls = StretchControls::new(0.5);
    controls.set_keylock(true);
    controls.set_backend(StretchKind::Signalsmith);
    let mut stage = processor(Arc::clone(&controls));
    let source = sine(TimeStretchProcessor::PRESENTATION_FRAMES);
    TempoStage::push_source(&mut stage, chunk(&source)).expect("old source quantum is admitted");
    while TempoStage::buffered_source_quanta(&stage) != 0 {
        let mut output =
            vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
        let credit = OutputCredit::new(
            &mut output,
            usize::from(Consts::CH),
            TimeStretchProcessor::PRESENTATION_FRAMES,
        );
        TempoStage::render(&mut stage, None, credit, &mut |_| {})
            .expect("old source quantum renders before the barrier");
    }
    let mut debt = TempoStage::begin_discontinuity(&mut stage)
        .expect("ordered discontinuity creates finite debt");
    loop {
        let mut output =
            vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
        let credit = OutputCredit::new(
            &mut output,
            usize::from(Consts::CH),
            TimeStretchProcessor::PRESENTATION_FRAMES,
        );
        if matches!(
            TempoStage::render_discontinuity(&mut stage, &mut debt, credit, &mut |_| {})
                .expect("old tail drains before replacement"),
            TempoDiscontinuityStep::Drained
        ) {
            break;
        }
    }

    controls.set_speed(1.0);
    controls.set_keylock(false);
    let replacement = PcmSpec::new(
        Consts::CH,
        NonZero::new(48_000).expect("replacement rate is non-zero"),
    );
    TempoStage::service_off_rt(
        &mut stage,
        TempoPrepareRequest::DecoderBoundary { spec: replacement },
    )
    .expect("replacement core prepares off-RT");
    let boundary = TempoStage::prepared_boundary(&stage)
        .expect("replacement core publishes an exact boundary");
    TempoStage::commit_prepared(&mut stage, boundary)
        .expect("drained stage adopts replacement PCM shape");

    assert_eq!(TempoStage::output_spec(&stage), replacement);
    assert!(Arc::ptr_eq(&stage.controls, &controls));
    assert_eq!(stage.controls.speed(), 1.0);
    assert!(!stage.controls.keylock());
    let replacement_source = vec![0.25; TimeStretchProcessor::PRESENTATION_FRAMES * 2];
    TempoStage::push_source(&mut stage, chunk_at(replacement, &replacement_source))
        .expect("first replacement source quantum is admitted");
    let mut output = vec![0.0; replacement_source.len()];
    let credit = OutputCredit::new(
        &mut output,
        usize::from(replacement.channels),
        TimeStretchProcessor::PRESENTATION_FRAMES,
    );
    let TempoStep::Rendered { frames, meta } =
        TempoStage::render(&mut stage, None, credit, &mut |_| {})
            .expect("replacement quantum renders with live controls")
    else {
        panic!("unity replacement quantum must render immediately");
    };
    assert_eq!(frames, TimeStretchProcessor::PRESENTATION_FRAMES);
    assert_eq!(meta.spec, replacement);
}
