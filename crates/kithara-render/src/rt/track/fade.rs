use std::{num::NonZeroU32, ops::Range};

use kithara_dsp::fade::{self, FadeCurve};
use num_traits::{ToPrimitive, cast};

use crate::{CrossfadeSettings, consts::FADE_CHUNK};

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
    /// Gain at the last session frame.
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
        &self,
        scratch_bufs: &mut [&mut [f32]],
        mix_bufs: &mut [&mut [f32]],
        range: Range<usize>,
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
        // From the fade's last frame on, every frame holds the final gain:
        // unity or silence.
        let last = self.frames.saturating_sub(1);
        let fading = usize::try_from(last.saturating_sub(self.frame))
            .unwrap_or(usize::MAX)
            .min(output_l.len());
        let (output_l, settled_l) = output_l.split_at_mut(fading);
        let (output_r, settled_r) = output_r.split_at_mut(fading);
        let (input_l, unity_l) = input_l.split_at(fading);
        let (input_r, unity_r) = input_r.split_at(fading);
        if !settled_l.is_empty() && self.gain_at(last) == 1.0 {
            kithara_dsp::add(unity_l, settled_l);
            kithara_dsp::add(unity_r, settled_r);
        }
        for (index, ((out_l, out_r), (in_l, in_r))) in output_l
            .chunks_mut(FADE_CHUNK)
            .zip(output_r.chunks_mut(FADE_CHUNK))
            .zip(input_l.chunks(FADE_CHUNK).zip(input_r.chunks(FADE_CHUNK)))
            .enumerate()
        {
            let offset = u64::try_from(index.saturating_mul(FADE_CHUNK)).unwrap_or(u64::MAX);
            let mut gain = [0.0; FADE_CHUNK];
            let gain = &mut gain[..out_l.len()];
            self.fading_gains(self.frame.saturating_add(offset), gain);
            kithara_dsp::mul_add(in_l, gain, out_l);
            kithara_dsp::mul_add(in_r, gain, out_r);
        }
    }

    /// The gains of the frames from `first`, all before the fade's last
    /// frame: `gain_at`'s operations in the same order.
    fn fading_gains(&self, first: u64, gain: &mut [f32]) {
        match self.direction {
            Direction::In => {
                self.crossfade(first, gain);
                kithara_dsp::affine(gain, 1.0 - self.from, self.from);
            }
            Direction::Out => {
                self.crossfade(first, gain);
                kithara_dsp::affine(gain, self.from, 0.0);
            }
            Direction::DeclickOut => {
                let first = cast::<u64, f32>(first.saturating_add(1)).unwrap_or(f32::MAX);
                let frames = cast::<u64, f32>(self.frames).unwrap_or(f32::MAX);
                kithara_dsp::ramp(gain, first, 1.0);
                kithara_dsp::div(gain, frames);
                kithara_dsp::affine(gain, -1.0, 1.0);
                kithara_dsp::affine(gain, self.from, 0.0);
            }
        }
    }

    /// The side of `CrossfadeSettings::gains` this fade sounds for the frames
    /// from `first`: the incoming gain on the way in, the outgoing one on the
    /// way out.
    fn crossfade(&self, first: u64, gain: &mut [f32]) {
        let frames = gain.len();
        self.positions(first, gain);
        let mut outgoing = [0.0; FADE_CHUNK];
        let mut incoming = [0.0; FADE_CHUNK];
        fade::gains(
            FadeCurve::from(self.settings.curve),
            gain,
            &mut outgoing[..frames],
            &mut incoming[..frames],
        );
        let selected = if matches!(self.direction, Direction::In) {
            &incoming[..frames]
        } else {
            &outgoing[..frames]
        };
        for (position, &selected) in gain.iter_mut().zip(selected.iter()) {
            let (out, into) = FadeCurve::Linear.compute_gains_0_to_1(*position);
            let linear = if matches!(self.direction, Direction::In) {
                into
            } else {
                out
            };
            *position = self.settings.depth.mul_add(selected - linear, linear);
        }
    }

    /// The curve position `u` of the frames from `first`, mapped through the
    /// pivot with `CrossfadeSettings::gains`' IEEE operations in the same
    /// order, so every `u` is bit-identical to the law's.
    fn positions(&self, first: u64, u: &mut [f32]) {
        let first = cast::<u64, f32>(first).unwrap_or(f32::MAX);
        let last = cast::<u64, f32>(self.frames.saturating_sub(1)).unwrap_or(f32::MAX);
        let position = self.settings.position;
        kithara_dsp::ramp(u, first, 1.0);
        kithara_dsp::div(u, last);
        let (rising, falling) = u.split_at_mut(u.partition_point(|&x| x <= position));
        kithara_dsp::div(rising, 2.0 * position);
        kithara_dsp::affine(falling, 1.0, -position);
        kithara_dsp::div(falling, 2.0 * (1.0 - position));
        kithara_dsp::affine(falling, 1.0, 0.5);
    }

    pub(super) fn advance(&mut self, frames: usize) {
        if frames == 0 {
            return;
        }
        let frames = u64::try_from(frames).unwrap_or(u64::MAX);
        self.gain = self.gain_at(self.frame.saturating_add(frames - 1));
        self.frame = self.frame.saturating_add(frames);
        self.settled = self.frame >= self.frames;
    }

    fn gain_at(&self, frame: u64) -> f32 {
        let progress = if self.frames <= 1 {
            1.0
        } else if matches!(self.direction, Direction::DeclickOut) {
            let frame = cast::<u64, f32>(frame.saturating_add(1)).unwrap_or(f32::MAX);
            let frames = cast::<u64, f32>(self.frames).unwrap_or(f32::MAX);
            (frame / frames).min(1.0)
        } else {
            let frame = cast::<u64, f32>(frame).unwrap_or(f32::MAX);
            let frames = cast::<u64, f32>(self.frames - 1).unwrap_or(f32::MAX);
            (frame / frames).min(1.0)
        };
        let (out, into) = self.settings.gains(progress);
        match self.direction {
            Direction::In => (1.0 - self.from).mul_add(into, self.from),
            Direction::Out => self.from * out,
            Direction::DeclickOut => self.from * (1.0 - progress),
        }
    }

    pub(super) fn play(&mut self, _sample_rate: NonZeroU32) {
        self.direction = Direction::In;
        self.frame = 1;
        self.frames = 0;
        self.settled = true;
        self.from = 1.0;
        self.gain = 1.0;
    }

    /// Starts a fade from the current session gain, so a fade that
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
        self.settled = self.frames == 0 && matches!(direction, Direction::In);
    }

    delegate::delegate! {
        to self {
            #[expr(self.gain)]
            /// Gain at the last session frame.
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

    pub(super) fn stop(&mut self) {
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
