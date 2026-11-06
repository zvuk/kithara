use std::{num::NonZeroU32, ops::Range};

use kithara_dsp::param::SmootherConfig;
use num_traits::ToPrimitive;

/// A track's transport: the ramp that lets its share into the mix on start and takes it out on
/// stop, so neither steps the waveform.
pub(super) struct TrackGate {
    level: f32,
    from: f32,
    target: f32,
    frame: usize,
    frames: usize,
    declick: SmootherConfig,
}

impl TrackGate {
    const OPEN: f32 = 1.0;
    const SHUT: f32 = 0.0;

    pub(super) fn new(open: bool, declick: SmootherConfig, sample_rate: NonZeroU32) -> Self {
        Self {
            level: Self::level(open),
            from: Self::level(open),
            target: Self::level(open),
            frame: 0,
            frames: crate::rt::config::declick_frame_count(declick, sample_rate).get(),
            declick,
        }
    }

    const fn level(open: bool) -> f32 {
        if open { Self::OPEN } else { Self::SHUT }
    }

    /// Ramps the gate open or shut from the next frame it passes.
    pub(super) fn steer(&mut self, open: bool) {
        self.from = self.level;
        self.target = Self::level(open);
        self.frame = 0;
    }

    /// Moves the gate to where it is steered at once.
    pub(super) fn snap(&mut self) {
        self.level = self.target;
        self.from = self.target;
        self.frame = self.frames;
    }

    pub(super) fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.frames = crate::rt::config::declick_frame_count(self.declick, sample_rate).get();
        self.from = self.level;
        self.frame = 0;
    }

    /// Whether the gate has shut: nothing passes it, so its track need not be read.
    pub(super) fn is_shut(&self) -> bool {
        self.level == Self::SHUT && self.target == Self::SHUT
    }

    /// Scales the frames of `range` in a stereo pair by the gate.
    pub(super) fn apply(&mut self, bufs: &mut [&mut [f32]], range: Range<usize>) {
        if self.level == Self::OPEN && self.target == Self::OPEN {
            return;
        }
        let [left, right, ..] = bufs else {
            return;
        };
        let (left, right) = (&mut left[range.clone()], &mut right[range]);
        let mut gains = [0.0; crate::consts::FADE_CHUNK];
        for (left, right) in left
            .chunks_mut(crate::consts::FADE_CHUNK)
            .zip(right.chunks_mut(crate::consts::FADE_CHUNK))
        {
            let gains = &mut gains[..left.len()];
            for gain in gains.iter_mut() {
                let progress = if self.frames <= 1 {
                    1.0
                } else {
                    self.frame.min(self.frames - 1).to_f32().unwrap_or(f32::MAX)
                        / (self.frames - 1).to_f32().unwrap_or(f32::MAX)
                };
                *gain = (self.target - self.from).mul_add(progress, self.from);
                self.level = *gain;
                self.frame = self.frame.saturating_add(1);
            }
            kithara_dsp::multiply_in_place(left, gains);
            kithara_dsp::multiply_in_place(right, gains);
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn gate_pcm_and_state_follow_integer_frame_positions() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        for frames in [0, 1, 300, (1 << 24) + 9] {
            let mut gate = TrackGate::new(false, SmootherConfig::default(), rate);
            gate.steer(true);
            gate.frames = frames;
            gate.frame = frames.saturating_sub(5);
            let mut frame = gate.frame;
            let mut level = gate.level;
            for length in [0, 1, 257, 513] {
                let mut left = vec![0.75_f32; 520];
                let mut right = vec![-0.0_f32; 519];
                let (mut expected_left, mut expected_right) = (left.clone(), right.clone());
                if level != 1.0 {
                    for (l, r) in expected_left[3..3 + length]
                        .iter_mut()
                        .zip(&mut expected_right[3..3 + length])
                    {
                        let progress = if frames <= 1 {
                            1.0
                        } else {
                            frame.min(frames - 1).to_f32().expect("frame")
                                / (frames - 1).to_f32().expect("length")
                        };
                        level = (gate.target - gate.from).mul_add(progress, gate.from);
                        frame = frame.saturating_add(1);
                        *l *= level;
                        *r *= level;
                    }
                }
                gate.apply(&mut [&mut left, &mut right], 3..3 + length);
                assert_eq!(
                    left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                    expected_left
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    right.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                    expected_right
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>()
                );
                assert_eq!(gate.level.to_bits(), level.to_bits());
                assert_eq!(gate.frame, frame);
            }
            gate.update_sample_rate(NonZeroU32::new(96_000).expect("rate"));
            assert_eq!(gate.frame, 0);
            assert_eq!(gate.from.to_bits(), level.to_bits());
        }
    }
}
