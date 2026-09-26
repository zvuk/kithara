use std::{num::NonZeroU32, ops::Range};

use num_traits::{ToPrimitive, cast};

use crate::CrossfadeSettings;

#[derive(Clone, Copy, Debug)]
enum Direction {
    In,
    Out,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct TrackFade {
    settings: CrossfadeSettings,
    direction: Direction,
    #[field(get, copy)]
    settled: bool,
    frame: u64,
    frames: u64,
}

impl TrackFade {
    pub(super) fn new(settings: CrossfadeSettings, sample_rate: NonZeroU32) -> Self {
        let frames = Self::frames(settings.duration, sample_rate);
        Self {
            settings,
            direction: Direction::In,
            frame: 0,
            frames,
            settled: frames == 0,
        }
    }

    pub(super) const fn duration(&self) -> f32 {
        self.settings.duration
    }

    pub(super) fn remaining_frames(&self) -> usize {
        usize::try_from(self.frames.saturating_sub(self.frame)).unwrap_or(usize::MAX)
    }

    pub(super) fn fade_in(&mut self, settings: CrossfadeSettings, sample_rate: NonZeroU32) {
        self.start(Direction::In, settings, sample_rate);
    }

    pub(super) fn fade_out(&mut self, settings: CrossfadeSettings, sample_rate: NonZeroU32) {
        self.start(Direction::Out, settings, sample_rate);
    }

    fn frames(duration: f32, sample_rate: NonZeroU32) -> u64 {
        let sample_rate = cast::<u32, f32>(sample_rate.get()).unwrap_or(f32::MAX);
        (duration * sample_rate)
            .round()
            .to_u64()
            .unwrap_or(u64::MAX)
    }

    pub(super) fn mix_range(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        _frames: usize,
    ) {
        const MIN_STEREO_CHANNELS: usize = 2;
        if scratch_bufs.len() < MIN_STEREO_CHANNELS || mix_bufs.len() < MIN_STEREO_CHANNELS {
            return;
        }
        let (output_l_slice, output_r_slice) = mix_bufs.split_at_mut(1);
        let output_l = &mut output_l_slice[0][range.clone()];
        let output_r = &mut output_r_slice[0][range.clone()];
        let input_l = &scratch_bufs[0][range.clone()];
        let input_r = &scratch_bufs[1][range];
        for ((out_l, out_r), (in_l, in_r)) in output_l
            .iter_mut()
            .zip(output_r.iter_mut())
            .zip(input_l.iter().zip(input_r))
        {
            let progress = if self.frames <= 1 {
                1.0
            } else {
                let frame = cast::<u64, f32>(self.frame).unwrap_or(f32::MAX);
                let frames = cast::<u64, f32>(self.frames - 1).unwrap_or(f32::MAX);
                (frame / frames).min(1.0)
            };
            let gains = self.settings.gains(progress);
            let gain = match self.direction {
                Direction::In => gains.1,
                Direction::Out => gains.0,
            };
            if gain == 1.0 {
                *out_l += in_l;
                *out_r += in_r;
            } else if gain != 0.0 {
                *out_l = in_l.mul_add(gain, *out_l);
                *out_r = in_r.mul_add(gain, *out_r);
            }
            self.frame = self.frame.saturating_add(1);
        }
        self.settled = self.frame >= self.frames;
    }

    pub(super) fn play(&mut self, _sample_rate: NonZeroU32) {
        self.direction = Direction::In;
        self.frame = 1;
        self.frames = 0;
        self.settled = true;
    }

    fn start(
        &mut self,
        direction: Direction,
        settings: CrossfadeSettings,
        sample_rate: NonZeroU32,
    ) {
        self.settings = settings;
        self.direction = direction;
        self.frame = 0;
        self.frames = Self::frames(settings.duration, sample_rate);
        self.settled = self.frames == 0;
    }

    pub(super) fn stop(&mut self, _sample_rate: NonZeroU32) {
        self.direction = Direction::Out;
        self.frame = 1;
        self.frames = 0;
        self.settled = true;
    }

    pub(super) fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        let progress = if self.frames <= 1 {
            1.0
        } else {
            let frame = cast::<u64, f64>(self.frame).unwrap_or(f64::MAX);
            let frames = cast::<u64, f64>(self.frames - 1).unwrap_or(f64::MAX);
            frame / frames
        };
        self.frames = Self::frames(self.settings.duration, sample_rate);
        self.frame = if self.frames <= 1 {
            self.frames
        } else {
            let frames = cast::<u64, f64>(self.frames - 1).unwrap_or(f64::MAX);
            (progress * frames).round().to_u64().unwrap_or(u64::MAX)
        };
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::CrossfadeCurve;

    #[kithara::test]
    fn rendered_linear_fade_reaches_both_exact_endpoints() {
        let settings = CrossfadeSettings::new(0.004, CrossfadeCurve::Linear, 1.0, 0.5)
            .expect("valid settings");
        let mut fade = TrackFade::new(settings, NonZeroU32::new(1_000).expect("nonzero"));
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

    #[kithara::test]
    fn correlated_overlap_sums_to_unity_and_old_starvation_does_not_stall_clock() {
        let rate = NonZeroU32::new(1_000).expect("fixture rate");
        let settings = CrossfadeSettings::new(0.004, CrossfadeCurve::Linear, 1.0, 0.5)
            .expect("fixture overlap");
        let mut incoming = TrackFade::new(settings, rate);
        incoming.fade_in(settings, rate);
        let mut outgoing = TrackFade::new(settings, rate);
        outgoing.fade_out(settings, rate);

        let mut new_left = [0.75; 4];
        let mut new_right = [0.75; 4];
        let mut old_left = [0.75; 4];
        let mut old_right = [0.75; 4];
        let mut left = [0.0; 4];
        let mut right = [0.0; 4];
        incoming.mix_range(
            &mut [&mut new_left, &mut new_right],
            &mut [&mut left, &mut right],
            0..4,
            4,
        );
        outgoing.mix_range(
            &mut [&mut old_left, &mut old_right],
            &mut [&mut left, &mut right],
            0..4,
            4,
        );
        assert!(left.iter().all(|sample| (sample - 0.75).abs() < 1e-6));
        assert_eq!(right, left);
        assert!(incoming.settled() && outgoing.settled());
        assert_eq!(outgoing.remaining_frames(), 0);

        let mut starving = TrackFade::new(settings, rate);
        starving.fade_out(settings, rate);
        let mut first = [0.75; 2];
        let mut first_right = [0.75; 2];
        let mut first_mix = [0.0; 2];
        let mut first_mix_right = [0.0; 2];
        starving.mix_range(
            &mut [&mut first, &mut first_right],
            &mut [&mut first_mix, &mut first_mix_right],
            0..2,
            2,
        );
        let mut silence = [0.0; 2];
        let mut silence_right = [0.0; 2];
        let mut second_mix = [0.0; 2];
        let mut second_mix_right = [0.0; 2];
        starving.mix_range(
            &mut [&mut silence, &mut silence_right],
            &mut [&mut second_mix, &mut second_mix_right],
            0..2,
            2,
        );
        assert!(starving.settled());
        assert_eq!(starving.remaining_frames(), 0);
        assert_eq!(second_mix, [0.0; 2]);
    }
}
