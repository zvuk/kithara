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
        for (l, r) in left[range.clone()].iter_mut().zip(right[range].iter_mut()) {
            let progress = if self.frames <= 1 {
                1.0
            } else {
                self.frame.min(self.frames - 1).to_f32().unwrap_or(f32::MAX)
                    / (self.frames - 1).to_f32().unwrap_or(f32::MAX)
            };
            let gain = (self.target - self.from).mul_add(progress, self.from);
            self.level = gain;
            self.frame = self.frame.saturating_add(1);
            *l *= gain;
            *r *= gain;
        }
    }
}
