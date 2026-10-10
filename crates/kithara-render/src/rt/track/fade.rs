use std::{num::NonZeroU32, ops::Range};

use num_traits::{ToPrimitive, cast};

use crate::CrossfadeSettings;

#[derive(Clone, Copy, Debug)]
enum Direction {
    In,
    Out,
    DeclickOut,
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
    /// Gain the current fade started from.
    from: f32,
    /// Gain applied to the last mixed frame.
    gain: f32,
}

/// A fade that holds its track silent until `fade_in` or `play` starts it.
impl Default for TrackFade {
    fn default() -> Self {
        Self {
            settings: CrossfadeSettings::default(),
            direction: Direction::Out,
            frame: 1,
            frames: 0,
            settled: true,
            from: 0.0,
            gain: 0.0,
        }
    }
}

impl TrackFade {
    pub(super) fn fade_in(&mut self, settings: CrossfadeSettings, sample_rate: NonZeroU32) {
        self.start(Direction::In, settings, sample_rate);
    }

    pub(super) fn fade_out(&mut self, settings: CrossfadeSettings, sample_rate: NonZeroU32) {
        self.start(Direction::Out, settings, sample_rate);
    }

    pub(super) fn declick_out(&mut self, settings: CrossfadeSettings, sample_rate: NonZeroU32) {
        self.start(Direction::DeclickOut, settings, sample_rate);
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
            } else if matches!(self.direction, Direction::DeclickOut) {
                let frame = cast::<u64, f32>(self.frame.saturating_add(1)).unwrap_or(f32::MAX);
                let frames = cast::<u64, f32>(self.frames).unwrap_or(f32::MAX);
                (frame / frames).min(1.0)
            } else {
                let frame = cast::<u64, f32>(self.frame).unwrap_or(f32::MAX);
                let frames = cast::<u64, f32>(self.frames - 1).unwrap_or(f32::MAX);
                (frame / frames).min(1.0)
            };
            let (out, into) = self.settings.gains(progress);
            let gain = match self.direction {
                Direction::In => (1.0 - self.from).mul_add(into, self.from),
                Direction::Out => self.from * out,
                Direction::DeclickOut => self.from * (1.0 - progress),
            };
            self.gain = gain;
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
        self.from = 1.0;
        self.gain = 1.0;
    }

    /// Starts a fade from the gain the last mixed frame had, so a fade that
    /// reverses another one continues from where it was instead of stepping.
    fn start(
        &mut self,
        direction: Direction,
        settings: CrossfadeSettings,
        sample_rate: NonZeroU32,
    ) {
        self.settings = settings;
        self.direction = direction;
        self.from = self.gain;
        self.frame = 0;
        self.frames = Self::frames(settings.duration, sample_rate);
        self.settled = self.frames == 0;
    }

    delegate::delegate! {
        to self {
            #[expr(self.gain)]
            /// Gain the envelope applied to the last mixed frame.
            pub(super) const fn gain(&self) -> f32;
        }
    }

    /// Whether the envelope is on its way down to silence.
    pub(super) const fn is_fading_out(&self) -> bool {
        matches!(self.direction, Direction::Out | Direction::DeclickOut) && !self.settled
    }

    /// Frames until the envelope settles.
    pub(super) const fn remaining(&self) -> u64 {
        self.frames.saturating_sub(self.frame)
    }

    pub(super) fn stop(&mut self, _sample_rate: NonZeroU32) {
        self.direction = Direction::Out;
        self.frame = 1;
        self.frames = 0;
        self.settled = true;
        self.from = 0.0;
        self.gain = 0.0;
    }

    pub(super) fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        let endpoint = match self.direction {
            Direction::DeclickOut => 0,
            Direction::In | Direction::Out => 1,
        };
        let steps = self.frames.saturating_sub(endpoint);
        let progress = if steps == 0 {
            1.0
        } else {
            let frame = cast::<u64, f64>(self.frame).unwrap_or(f64::MAX);
            let steps = cast::<u64, f64>(steps).unwrap_or(f64::MAX);
            frame / steps
        };
        self.frames = Self::frames(self.settings.duration, sample_rate);
        let steps = self.frames.saturating_sub(endpoint);
        self.frame = if steps == 0 {
            self.frames
        } else {
            let steps = cast::<u64, f64>(steps).unwrap_or(f64::MAX);
            (progress * steps).round().to_u64().unwrap_or(u64::MAX)
        };
    }
}
