use std::ops::Range;

#[rustfmt::skip]
use firewheel::dsp::filter::smoothing_filter::DEFAULT_SETTLE_EPSILON;
#[rustfmt::skip]
use firewheel::param::smoother::SmootherConfig;
use std::num::NonZeroU32;

use firewheel::dsp::{
    fade::FadeCurve,
    mix::{Mix, MixDSP},
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct TrackFade {
    curve: FadeCurve,
    mix: MixDSP,
    #[field(get, vis = "pub(super)")]
    duration: f32,
}

impl TrackFade {
    pub(super) fn new(duration: f32, curve: FadeCurve, sample_rate: NonZeroU32) -> Self {
        Self {
            curve,
            duration,
            mix: MixDSP::new(
                Mix::FULLY_WET,
                curve,
                Self::smoother_config(duration),
                sample_rate,
            ),
        }
    }

    pub(super) fn fade_in(&mut self) {
        self.mix.set_mix(Mix::FULLY_DRY, self.curve);
    }

    pub(super) fn fade_out(&mut self) {
        self.mix.set_mix(Mix::FULLY_WET, self.curve);
    }

    pub(super) fn has_settled(&self) -> bool {
        self.mix.has_settled()
    }

    pub(super) fn mix_range(
        &mut self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
        frames: usize,
    ) {
        const MIN_STEREO_CHANNELS: usize = 2;
        if scratch_bufs.len() < MIN_STEREO_CHANNELS || mix_bufs.len() < MIN_STEREO_CHANNELS {
            return;
        }

        let (output_l_slice, output_r_slice) = mix_bufs.split_at_mut(1);
        let output_l = &mut output_l_slice[0][range.clone()];
        let output_r = &mut output_r_slice[0][range.clone()];

        self.mix.mix_dry_into_wet_stereo(
            &scratch_bufs[0][range.clone()],
            &scratch_bufs[1][range],
            output_l,
            output_r,
            frames,
        );
    }

    /// Take the track to full level.
    ///
    /// A settled mix snaps, so a start with no crossfade stays sample-exact.
    /// A fade still in flight keeps its ramp and reaches full level on its own
    /// schedule instead of stepping there.
    pub(super) fn play(&mut self) {
        let settled = self.mix.has_settled();
        self.mix.set_mix(Mix::FULLY_DRY, self.curve);
        if settled {
            self.mix.reset_to_target();
        }
    }

    fn smoother_config(duration: f32) -> SmootherConfig {
        SmootherConfig {
            smooth_seconds: duration,
            settle_epsilon: DEFAULT_SETTLE_EPSILON,
        }
    }

    pub(super) fn stop(&mut self) {
        self.mix.set_mix(Mix::FULLY_WET, self.curve);
        self.mix.reset_to_target();
    }

    /// Re-arm the mix for a new fade length.
    ///
    /// The new smoother starts at the state's end point, which costs a fade in
    /// flight its ramp, so an unchanged duration only follows the sample rate.
    pub(super) fn update_duration(
        &mut self,
        duration: f32,
        sample_rate: NonZeroU32,
        leading: bool,
    ) {
        if (duration - self.duration).abs() < f32::EPSILON {
            self.mix.update_sample_rate(sample_rate);
            return;
        }

        let target_mix = if leading {
            Mix::FULLY_DRY
        } else {
            Mix::FULLY_WET
        };
        self.mix = MixDSP::new(
            target_mix,
            self.curve,
            Self::smoother_config(duration),
            sample_rate,
        );
        self.duration = duration;
    }
}
