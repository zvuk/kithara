use kithara_stretch::StretchKind;
use kithara_test_utils::kithara;

use super::{
    playback::{render, vinyl},
    renderer,
};
#[cfg(feature = "stretch-glide")]
use crate::WarpCapabilities;
use crate::{Pitch, WarpConfig, consts};

#[cfg(feature = "stretch-glide")]
#[kithara::test]
fn glide_masks_unsupported_keylock_from_its_prepared_engine() {
    let renderer = renderer(
        &WarpConfig::builder()
            .backend(StretchKind::Glide)
            .pitch(Pitch::HELD)
            .speed(0.5)
            .build(),
    );

    assert!(renderer.requested_pitch.keylock());
    assert_eq!(renderer.current_kind, StretchKind::Glide);
    assert!(!renderer.current_pitch_engine);
    assert_eq!(
        renderer
            .engine
            .as_ref()
            .expect("Glide is compiled")
            .capabilities()
            .functions(),
        WarpCapabilities::RATE
    );
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-glide"))]
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-glide", case::glide(StretchKind::Glide))]
fn vinyl_varispeed_preserves_periodic_attack_positions(#[case] backend: StretchKind) {
    const SPEED: f32 = 1.25;
    const SPEED_NUMERATOR: usize = 5;
    const SPEED_DENOMINATOR: usize = 4;
    const PERIOD: usize = 4_096;
    const FRAMES: usize = PERIOD * 6;

    let mut input = vec![0.0; FRAMES * usize::from(consts::CH)];
    for frame in (PERIOD..FRAMES).step_by(PERIOD) {
        input[frame * usize::from(consts::CH)] = 1.0;
        input[frame * usize::from(consts::CH) + 1] = 1.0;
    }

    let output = render(&mut vinyl(backend, SPEED), &input);
    let mono: Vec<f32> = output
        .iter()
        .step_by(usize::from(consts::CH))
        .copied()
        .collect();
    for source_frame in (PERIOD..FRAMES).step_by(PERIOD) {
        let expected =
            (2 * source_frame * SPEED_DENOMINATOR + SPEED_NUMERATOR) / (2 * SPEED_NUMERATOR);
        let start = expected.saturating_sub(256);
        let end = expected.saturating_add(257).min(mono.len());
        let actual = mono[start..end]
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.abs().total_cmp(&right.abs()))
            .map(|(offset, _)| start + offset)
            .expect("attack search window is non-empty");
        assert!(
            actual.abs_diff(expected) <= 1,
            "vinyl attack moved from frame {expected} to {actual}"
        );
    }
}

#[kithara::test]
#[cfg(feature = "stretch-signalsmith")]
fn vinyl_accepts_rates_below_one_quarter_when_the_engine_declares_them() {
    const SPEED: f32 = 0.125;
    const FRAMES: usize = 1_024;

    let mut input = vec![0.0; FRAMES * usize::from(consts::CH)];
    input[usize::from(consts::CH)..usize::from(consts::CH) * 2].fill(1.0);

    let output = render(&mut vinyl(StretchKind::Signalsmith, SPEED), &input);

    assert_eq!(output.len() / usize::from(consts::CH), FRAMES * 8);
}
