use std::num::NonZeroU32;

use kithara_test_utils::kithara;
use num_traits::cast;

use crate::{CrossfadeSettings, rt::track::fade::TrackFade};

#[kithara::test]
fn a_declick_stop_keeps_its_consumed_fraction_when_the_rate_changes() {
    let rate = NonZeroU32::new(1_000).expect("rate");
    let mut fade = TrackFade::default();
    fade.play(rate);
    fade.declick_out(
        CrossfadeSettings {
            duration: 0.004,
            ..Default::default()
        },
        rate,
    );
    let mut left = [1.0; 6];
    let mut right = left;
    let mut output_left = [0.0; 6];
    let mut output_right = output_left;
    fade.mix_range(
        &mut [&mut left, &mut right],
        &mut [&mut output_left, &mut output_right],
        0..2,
        6,
    );
    fade.update_sample_rate(NonZeroU32::new(2_000).expect("rate"));
    assert_eq!(fade.remaining(), 4);
    fade.mix_range(
        &mut [&mut left, &mut right],
        &mut [&mut output_left, &mut output_right],
        2..6,
        6,
    );
    assert_eq!(output_left, [0.75, 0.5, 0.375, 0.25, 0.125, 0.0]);
    assert_eq!(output_right, output_left);
    assert!(fade.settled());
}

#[kithara::test]
fn a_declick_stop_changes_its_first_frame_and_reaches_silence_on_its_last() {
    let rate = NonZeroU32::new(1_000).expect("rate");
    let mut fade = TrackFade::default();
    fade.play(rate);
    fade.declick_out(
        CrossfadeSettings {
            duration: 0.004,
            ..Default::default()
        },
        rate,
    );
    let mut left = [1.0; 4];
    let mut right = left;
    let mut output_left = [0.0; 4];
    let mut output_right = output_left;
    fade.mix_range(
        &mut [&mut left, &mut right],
        &mut [&mut output_left, &mut output_right],
        0..4,
        4,
    );
    assert_eq!(output_left, [0.75, 0.5, 0.25, 0.0]);
    assert_eq!(output_right, output_left);
    assert_eq!(fade.remaining(), 0);
    assert!(fade.settled());
}
use crate::CrossfadeCurve;

#[kithara::test]
fn rendered_linear_fade_reaches_both_exact_endpoints() {
    let settings =
        CrossfadeSettings::new(0.004, CrossfadeCurve::Linear, 1.0, 0.5).expect("valid settings");
    let mut fade = TrackFade::default();
    fade.fade_in(settings, NonZeroU32::new(1_000).expect("nonzero"));
    let mut input_l = [1.0; 4];
    let mut input_r = [1.0; 4];
    let mut output_l = [0.0; 4];
    let mut output_r = [0.0; 4];
    fade.mix_range(
        &mut [&mut input_l, &mut input_r],
        &mut [&mut output_l, &mut output_r],
        0..4,
        4,
    );
    assert_eq!(output_l[0], 0.0);
    assert_eq!(output_l[3], 1.0);
    assert_eq!(output_l, output_r);
    assert!(fade.settled());
}

/// Frame `i` of a fade over `n` frames sounds `gains(i / (n − 1))` of its input: the incoming
/// gain on the way up from silence, the outgoing one on the way down from full level.
#[kithara::test]
fn a_fade_sounds_its_crossfade_gains_frame_by_frame() {
    const LAW_FRAMES: usize = 16;

    fn sounded(fade: &mut TrackFade) -> [f32; LAW_FRAMES] {
        let mut input_l = [1.0; LAW_FRAMES];
        let mut input_r = [1.0; LAW_FRAMES];
        let mut output_l = [0.0; LAW_FRAMES];
        let mut output_r = [0.0; LAW_FRAMES];
        fade.mix_range(
            &mut [&mut input_l, &mut input_r],
            &mut [&mut output_l, &mut output_r],
            0..LAW_FRAMES,
            LAW_FRAMES,
        );
        output_l
    }

    let sample_rate = NonZeroU32::new(1_000).expect("nonzero");
    let settings = CrossfadeSettings::new(0.016, CrossfadeCurve::EqualPower, 0.7, 0.3)
        .expect("valid settings");
    let law = |frame: usize| {
        let frame = cast::<usize, f32>(frame).unwrap_or(f32::MAX);
        let last = cast::<usize, f32>(LAW_FRAMES - 1).unwrap_or(f32::MAX);
        settings.gains(frame / last)
    };
    let mut fade = TrackFade::default();

    fade.fade_in(settings, sample_rate);
    let rising = sounded(&mut fade);
    fade.fade_out(settings, sample_rate);
    let falling = sounded(&mut fade);

    for frame in 0..LAW_FRAMES {
        let (out, into) = law(frame);
        assert_eq!(rising[frame], into, "fade-in frame {frame}");
        assert_eq!(falling[frame], out, "fade-out frame {frame}");
    }
}
