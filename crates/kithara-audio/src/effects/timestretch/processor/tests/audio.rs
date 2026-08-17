use std::{collections::HashSet, num::NonZero};

use kithara_decode::{DecodeError, PcmSpec};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stretch::{StretchBackend, StretchBackendError, StretchKind};
use kithara_test_utils::kithara;

use super::{
    super::*,
    common::{
        Consts, assert_half_speed_contract, assert_unity_contract, chunk, dominant_bin, drain_eof,
        expected_bin, keylocked, processor, render_chunk, run_vinyl, sine,
    },
};

#[cfg(feature = "stretch-signalsmith")]
#[kithara::test]
fn signalsmith_half_speed_and_unity_contracts() {
    assert_half_speed_contract(StretchKind::Signalsmith);
    assert_unity_contract(StretchKind::Signalsmith);
}
#[cfg(feature = "stretch-bungee")]
#[kithara::test]
fn bungee_half_speed_and_unity_contracts() {
    assert_half_speed_contract(StretchKind::Bungee);
    assert_unity_contract(StretchKind::Bungee);
}

#[kithara::test]
fn output_meta_preserves_decoder_timeline() {
    let channels = usize::from(Consts::CH);
    let mut fx = keylocked(StretchKind::default(), 0.5);
    let cf = 1024usize;
    let block = sine(cf);
    let mut fed_ends = HashSet::new();
    let mut emitted = Vec::new();
    for i in 0..40u64 {
        let mut c = chunk(&block);
        let end = Duration::from_millis(i * 100 + 100);
        c.meta.timestamp = Duration::from_millis(i * 100);
        c.meta.end_timestamp = end;
        c.meta.frame_offset = i * u64::try_from(cf).unwrap();
        fed_ends.insert(end);
        if let Some(o) = render_chunk(&mut fx, c).expect("fixture stretch processing must succeed")
        {
            emitted.push(o);
        }
    }
    while let Some(o) = drain_eof(&mut fx) {
        emitted.push(o);
    }
    assert!(!emitted.is_empty(), "stretch produced no output");
    for o in &emitted {
        assert_eq!(
            o.spec(),
            PcmSpec {
                channels: Consts::CH,
                sample_rate: NonZero::new(Consts::SR).unwrap()
            },
            "spec (incl. sample rate) preserved verbatim"
        );
        assert_eq!(
            usize::try_from(o.meta.frames).unwrap(),
            o.samples.len() / channels,
            "frames recomputed to the actual output count"
        );
        assert!(
            fed_ends.contains(&o.meta.end_timestamp),
            "end_timestamp carried verbatim from an input chunk (source-track time)"
        );
    }
}

#[kithara::test]
fn vinyl_speed_scales_duration_and_pitch() {
    let channels = usize::from(Consts::CH);
    let in_frames = usize::try_from(Consts::SR).unwrap() * 2;
    let out = run_vinyl(StretchKind::default(), 2.0, in_frames);
    let out_frames = out.len() / channels;
    assert!(
        out_frames * 10 >= in_frames * 4 && out_frames * 10 <= in_frames * 6,
        "vinyl 2x should roughly halve duration, got {out_frames} from {in_frames}"
    );
    let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
    assert!(
        mono.len() >= Consts::N,
        "not enough vinyl output for the FFT window"
    );
    let peak = dominant_bin(&mono);
    let want = expected_bin(Consts::F0 * 2.0);
    assert!(
        peak.abs_diff(want) <= 4,
        "vinyl pitch did not follow speed: peak bin {peak}, expected {want}"
    );
}

#[kithara::test]
fn vinyl_speed_above_two_is_applied_without_clamping() {
    let channels = usize::from(Consts::CH);
    let in_frames = usize::try_from(Consts::SR).unwrap() * 2;
    let out = run_vinyl(StretchKind::default(), 4.0, in_frames);
    let out_frames = out.len() / channels;
    assert!(
        out_frames * 10 >= in_frames * 2 && out_frames * 10 <= in_frames * 3,
        "vinyl 4x should roughly quarter duration, got {out_frames} from {in_frames}"
    );
    let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
    assert!(
        mono.len() >= Consts::N,
        "not enough 4x vinyl output for the FFT window"
    );
    let peak = dominant_bin(&mono);
    let want = expected_bin(Consts::F0 * 4.0);
    assert!(
        peak.abs_diff(want) <= 6,
        "vinyl pitch was clamped above 2x: peak bin {peak}, expected {want}"
    );
}

#[kithara::test]
fn live_speed_change_updates_stretch_duration() {
    let controls = StretchControls::new(1.0);
    controls.set_keylock(true);
    controls.set_backend(StretchKind::default());
    let mut fx = processor(Arc::clone(&controls));
    let block = sine(4096);
    let unity = render_chunk(&mut fx, chunk(&block))
        .expect("unity bypass processing succeeds")
        .expect("unity bypass emits");
    assert_eq!(&unity.samples[..], &block[..], "unity phase bypasses");

    controls.set_speed(0.5);
    let mut stretched: Vec<f32> = Vec::new();
    for _ in 0..24 {
        if let Some(c) =
            render_chunk(&mut fx, chunk(&block)).expect("fixture stretch processing must succeed")
        {
            stretched.extend_from_slice(&c.samples);
        }
    }
    while let Some(c) = drain_eof(&mut fx) {
        stretched.extend_from_slice(&c.samples);
    }
    assert!(
        stretched.len() > block.len() * 24,
        "half-speed key-lock should lengthen output after a live speed change"
    );
}

#[kithara::test]
fn held_source_frames_tracks_only_active_stretch() {
    let controls = StretchControls::new(0.5);
    controls.set_keylock(true);
    controls.set_backend(StretchKind::default());
    let mut fx = processor(Arc::clone(&controls));
    let block = sine(4096);

    let _ = render_chunk(&mut fx, chunk(&block));
    assert!(
        TempoStage::held_source_frames(&fx) > 0,
        "non-unity stretch must retain source input"
    );

    controls.set_speed(1.0);
    let unity = render_chunk(&mut fx, chunk(&block))
        .expect("unity bypass processing succeeds")
        .expect("unity bypass emits");
    assert!(unity.samples.len() >= block.len());
    assert_eq!(
        &unity.samples[unity.samples.len() - block.len()..],
        &block[..]
    );
    assert_eq!(TempoStage::held_source_frames(&fx), 0);
}

struct MissingPromisedTailBackend;

impl StretchBackend for MissingPromisedTailBackend {
    fn flush(&mut self, _out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        Ok(())
    }

    fn max_output_samples(&self, input_frames: usize) -> usize {
        input_frames.saturating_mul(usize::from(Consts::CH))
    }

    fn max_tail_samples(&self) -> usize {
        0
    }

    fn source_latency_frames(&self) -> usize {
        1
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchBackendError> {
        out.extend_from_slice(input);
        Ok(())
    }

    fn reset(&mut self) {}

    fn set_pitch(&mut self, _scale: f64) -> Result<(), StretchBackendError> {
        Ok(())
    }

    fn set_ratio(&mut self, _stretch: f64) -> Result<(), StretchBackendError> {
        Ok(())
    }
}

#[kithara::test]
fn promised_tail_backend_missing_held_audio_fails_closed() {
    let mut stage = keylocked(StretchKind::default(), 0.5);
    let core = stage.active.as_mut().expect("fixture has an active core");
    core.backend = Some(Box::new(MissingPromisedTailBackend));
    core.processing = true;
    core.source_frames_pushed = 1;

    let Err(error) = TempoStage::finish_eof(&mut stage) else {
        panic!("promised held audio cannot disappear at EOF");
    };

    assert!(matches!(
        error,
        DecodeError::InvalidData {
            detail: "tempo backend retained source without a rendered drain tail"
        }
    ));
}

#[cfg(feature = "stretch-bungee")]
fn rendered_bungee_stage() -> TimeStretchProcessor {
    let mut stage = keylocked(StretchKind::Bungee, 0.5);
    let source = sine(TimeStretchProcessor::PRESENTATION_FRAMES);
    TempoStage::push_source(&mut stage, chunk(&source)).expect("one Bungee quantum is admitted");
    while TempoStage::buffered_source_quanta(&stage) != 0 {
        let mut output =
            vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
        let credit = OutputCredit::new(
            &mut output,
            usize::from(Consts::CH),
            TimeStretchProcessor::PRESENTATION_FRAMES,
        );
        TempoStage::render(&mut stage, None, credit, &mut |_| {})
            .expect("Bungee source quantum renders before the boundary");
    }
    assert!(
        TempoStage::held_source_frames(&stage) > 0,
        "fixture must exercise Bungee's undiscoverable held tail"
    );
    stage
}

#[cfg(feature = "stretch-bungee")]
#[kithara::test]
fn bungee_eof_discards_undrainable_held_source_without_failure() {
    let mut stage = rendered_bungee_stage();
    let mut debt = TempoStage::finish_eof(&mut stage)
        .expect("Bungee explicitly retires its undiscoverable tail at EOF");
    assert_eq!(TempoStage::held_source_frames(&stage), 0);
    let mut output = vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
    let credit = OutputCredit::new(
        &mut output,
        usize::from(Consts::CH),
        TimeStretchProcessor::PRESENTATION_FRAMES,
    );

    let step = TempoStage::render_eof(&mut stage, &mut debt, credit, &mut |_| {})
        .expect("Bungee EOF debt completes without a synthetic tail");

    assert!(matches!(step, TempoEofStep::Drained));
}

#[cfg(feature = "stretch-bungee")]
#[kithara::test]
fn bungee_barrier_discards_undrainable_held_source_without_failure() {
    let mut stage = rendered_bungee_stage();
    let mut debt = TempoStage::begin_discontinuity(&mut stage)
        .expect("Bungee explicitly retires its undiscoverable tail at a decoder barrier");
    assert_eq!(TempoStage::held_source_frames(&stage), 0);
    let mut output = vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
    let credit = OutputCredit::new(
        &mut output,
        usize::from(Consts::CH),
        TimeStretchProcessor::PRESENTATION_FRAMES,
    );

    let step = TempoStage::render_discontinuity(&mut stage, &mut debt, credit, &mut |_| {})
        .expect("Bungee barrier debt completes without a synthetic tail");

    assert!(matches!(step, TempoDiscontinuityStep::Drained));
}

#[cfg(feature = "stretch-signalsmith")]
#[kithara::test]
fn signalsmith_tail_drain_releases_held_source_frames() {
    let mut fx = keylocked(StretchKind::Signalsmith, 0.5);
    let block = sine(4096);
    let _ = render_chunk(&mut fx, chunk(&block));
    assert!(TempoStage::held_source_frames(&fx) > 0);

    let tail = drain_eof(&mut fx).expect("Signalsmith must emit its tail once");

    assert!(!tail.samples.is_empty());
    assert_eq!(TempoStage::held_source_frames(&fx), 0);
}

#[kithara::test]
fn live_keylock_toggle_switches_pitch_mode() {
    let controls = StretchControls::new(0.5);
    controls.set_keylock(false);
    controls.set_backend(StretchKind::default());
    let mut fx = processor(Arc::clone(&controls));
    let block = sine(4096);

    let mut vinyl_out: Vec<f32> = Vec::new();
    for _ in 0..24 {
        if let Some(c) =
            render_chunk(&mut fx, chunk(&block)).expect("fixture stretch processing must succeed")
        {
            vinyl_out.extend_from_slice(&c.samples);
        }
    }
    let vinyl_mono: Vec<f32> = vinyl_out
        .iter()
        .step_by(usize::from(Consts::CH))
        .copied()
        .collect();
    assert!(
        dominant_bin(&vinyl_mono).abs_diff(expected_bin(Consts::F0 * 0.5)) <= 4,
        "off: vinyl pitch follows speed"
    );

    controls.set_keylock(true);
    let mut stretched: Vec<f32> = Vec::new();
    for _ in 0..24 {
        if let Some(c) =
            render_chunk(&mut fx, chunk(&block)).expect("fixture stretch processing must succeed")
        {
            stretched.extend_from_slice(&c.samples);
        }
    }
    while let Some(c) = drain_eof(&mut fx) {
        stretched.extend_from_slice(&c.samples);
    }
    let mono: Vec<f32> = stretched
        .iter()
        .step_by(usize::from(Consts::CH))
        .copied()
        .collect();
    assert!(
        mono.len() >= Consts::N,
        "on: not enough output for the FFT window"
    );
    assert!(
        dominant_bin(&mono).abs_diff(expected_bin(Consts::F0)) <= 3,
        "on: pitch preserved after live toggle"
    );
}

#[cfg(all(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
fn live_backend_swap_continues_and_keeps_pitch() {
    let controls = StretchControls::new(0.5);
    controls.set_keylock(true);
    controls.set_backend(StretchKind::Bungee);
    let mut fx = processor(Arc::clone(&controls));
    let block = sine(4096);
    let mut out: Vec<f32> = Vec::new();
    for i in 0..24 {
        if i == 6 {
            controls.set_backend(StretchKind::Signalsmith);
        }
        if let Some(c) =
            render_chunk(&mut fx, chunk(&block)).expect("fixture stretch processing must succeed")
        {
            out.extend_from_slice(&c.samples);
        }
    }
    while let Some(c) = drain_eof(&mut fx) {
        out.extend_from_slice(&c.samples);
    }
    let mono: Vec<f32> = out
        .iter()
        .step_by(usize::from(Consts::CH))
        .copied()
        .collect();
    assert!(
        mono.len() >= Consts::N,
        "not enough output after swap for the FFT window"
    );
    assert!(
        dominant_bin(&mono).abs_diff(expected_bin(Consts::F0)) <= 3,
        "pitch preserved after live backend swap"
    );
}

#[kithara::test]
#[case(0.05)]
#[case(0.5)]
#[case(1.0)]
#[case(1.5)]
#[case(2.0)]
fn tempo_stage_speed_matrix_never_exceeds_one_output_credit(#[case] speed: f32) {
    for keylock in [false, true] {
        let controls = StretchControls::new(speed);
        controls.set_keylock(keylock);
        controls.set_backend(StretchKind::default());
        let mut stage = processor(controls);
        let source = sine(TimeStretchProcessor::PRESENTATION_FRAMES);
        TempoStage::push_source(&mut stage, chunk(&source))
            .expect("one source quantum is admitted");
        let mut retired = 0;
        let mut rendered = 0;
        for _ in 0..64 {
            if TempoStage::buffered_source_quanta(&stage) == 0 {
                break;
            }
            let mut output =
                vec![0.0; TimeStretchProcessor::PRESENTATION_FRAMES * usize::from(Consts::CH)];
            let credit = OutputCredit::new(
                &mut output,
                usize::from(Consts::CH),
                TimeStretchProcessor::PRESENTATION_FRAMES,
            );
            if let TempoStep::Rendered { frames, .. } =
                TempoStage::render(&mut stage, None, credit, &mut |chunk| {
                    retired += chunk.frames()
                })
                .expect("one credited tempo step succeeds")
            {
                assert!(frames <= TimeStretchProcessor::PRESENTATION_FRAMES);
                rendered += frames;
            }
        }

        assert_eq!(TempoStage::buffered_source_quanta(&stage), 0);
        assert_eq!(retired, TimeStretchProcessor::PRESENTATION_FRAMES);
        assert!(rendered > 0, "speed {speed}, keylock {keylock}");
    }
}
