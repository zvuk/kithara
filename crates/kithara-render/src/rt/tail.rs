use std::ops::Range;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_signal::FrameCount;

use super::track::PlayerTrack;
use crate::bridge::RtMetrics;

/// What a slot still sounds of the consumer a `Replace` took out of it: its next frames, ramped
/// down to silence, played under the consumer that took its place.
pub(crate) struct SlotTail {
    left: SampleBuffer,
    right: SampleBuffer,
    incoming_left: SampleBuffer,
    incoming_right: SampleBuffer,
    /// Frames the tail holds.
    len: usize,
    /// Frames of it already mixed.
    pos: usize,
}

impl SlotTail {
    /// A silent tail of `frames` frames, allocated once from `pools`.
    pub(crate) fn new<S>(pools: &PoolRegion<S>, frames: FrameCount) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        Ok(Self {
            left: pools.get_with_len::<f32>(frames.get())?,
            right: pools.get_with_len::<f32>(frames.get())?,
            incoming_left: pools.get_with_len::<f32>(frames.get())?,
            incoming_right: pools.get_with_len::<f32>(frames.get())?,
            len: 0,
            pos: 0,
        })
    }

    /// Fill the tail from `track`'s next frames, ramped from its gain down to silence.
    pub(crate) fn fill(
        &mut self,
        track: &mut PlayerTrack,
        frames: usize,
        metrics: &RtMetrics,
        budget: &mut usize,
    ) {
        let remaining = self.len - self.pos;
        self.left.copy_within(self.pos..self.len, 0);
        self.right.copy_within(self.pos..self.len, 0);
        let frames = frames.min(self.left.len()).min(self.right.len());
        let written = track.read_tail(
            &mut self.incoming_left[..frames],
            &mut self.incoming_right[..frames],
            metrics,
            budget,
        );
        self.len = remaining.max(written);
        self.left[remaining..self.len].fill(0.0);
        self.right[remaining..self.len].fill(0.0);
        for index in 0..written {
            self.left[index] += self.incoming_left[index];
            self.right[index] += self.incoming_right[index];
        }
        self.pos = 0;
    }

    pub(crate) fn advance(&mut self, frames: usize) {
        self.pos = self.pos.saturating_add(frames).min(self.len);
    }

    /// Add the tail's next frames into `range` of a stereo pair.
    pub(crate) fn mix(&mut self, left: &mut [f32], right: &mut [f32], range: Range<usize>) {
        let frames = range.len().min(self.len - self.pos);
        if frames == 0 {
            return;
        }
        let from = self.pos..self.pos + frames;
        let to = range.start..range.start + frames;
        for (out, tail) in left[to.clone()].iter_mut().zip(&self.left[from.clone()]) {
            *out += tail;
        }
        for (out, tail) in right[to].iter_mut().zip(&self.right[from]) {
            *out += tail;
        }
        self.pos += frames;
    }

    /// Whether frames are left to mix.
    pub(crate) const fn is_sounding(&self) -> bool {
        self.pos < self.len
    }

    pub(crate) fn capacity(&self) -> usize {
        self.left.len().min(self.right.len())
    }
}
