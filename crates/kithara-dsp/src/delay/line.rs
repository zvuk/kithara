use std::{
    num::NonZeroUsize,
    ops::{Deref, Range},
};

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use num_traits::ToPrimitive;

use super::DelayError;

/// Planar frame history with one write cursor shared by every channel.
///
/// Reads come before the write of the same frames: a delay of `d` frames
/// returns what was written `d` frames before the next write, so the valid
/// delays are `1..=capacity` and zero is refused. A span of `n` frames read at
/// delay `d` needs `n ≤ d`, which keeps every frame it touches already written;
/// a feedback loop therefore runs in spans no longer than its delay. A fresh
/// or reset line holds silence. Storage is taken once from the caller's
/// region in [`new`](Self::new); reading, writing and resetting never
/// allocate.
pub struct DelayLine {
    samples: SampleBuffer,
    channels: NonZeroUsize,
    capacity: NonZeroUsize,
    cursor: usize,
}

impl std::fmt::Debug for DelayLine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DelayLine")
            .field("channels", &self.channels)
            .field("capacity", &self.capacity)
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

/// A read position validated against one line's capacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tap {
    whole: usize,
    fraction: f32,
}

impl Tap {
    /// Whole frames of the delay; also the longest span it reads.
    #[must_use]
    pub const fn whole(self) -> usize {
        self.whole
    }

    /// Oldest frame the tap touches: the whole delay, or one past it when a
    /// fraction reaches the older neighbour.
    fn reach(self) -> usize {
        if self.fraction == 0.0 {
            self.whole
        } else {
            self.whole.wrapping_add(1)
        }
    }
}

impl DelayLine {
    /// Holds `capacity` frames of each of `channels` planes, taken from
    /// `pools`.
    ///
    /// # Errors
    /// [`PoolError`] when `channels × capacity` samples overflow or do not
    /// fit the region.
    pub fn new<S>(
        channels: NonZeroUsize,
        capacity: NonZeroUsize,
        pools: &PoolRegion<S>,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        Ok(Self {
            samples: pools.get_with_len::<f32>(channels.get().saturating_mul(capacity.get()))?,
            channels,
            capacity,
            cursor: 0,
        })
    }

    #[must_use]
    pub const fn channels(&self) -> NonZeroUsize {
        self.channels
    }

    /// Longest readable delay in frames.
    #[must_use]
    pub const fn capacity(&self) -> NonZeroUsize {
        self.capacity
    }

    /// A position `frames` behind the next write.
    ///
    /// # Errors
    /// [`DelayError::OutOfRange`] unless `frames` lies in
    /// `1..=`[`capacity`](Self::capacity); `NaN` and infinities lie outside.
    pub fn tap(&self, frames: f64) -> Result<Tap, DelayError> {
        let capacity = self.capacity.get().to_f64().unwrap_or(0.0);
        if !(1.0..=capacity).contains(&frames) {
            return Err(DelayError::OutOfRange);
        }
        let whole = frames.floor();
        Ok(Tap {
            whole: whole.to_usize().ok_or(DelayError::OutOfRange)?,
            fraction: (frames - whole).to_f32().ok_or(DelayError::OutOfRange)?,
        })
    }

    /// Fills `output` with `channel` at `tap`, starting at the next write.
    ///
    /// A fractional tap is the chord between the two neighbouring frames,
    /// `newer + fraction · (older − newer)`.
    ///
    /// # Errors
    /// [`DelayError::OutOfRange`] when `tap` reaches past this line's
    /// capacity, as a tap made by a longer line can;
    /// [`DelayError::Shape`] when `channel` is out of range or `output` is
    /// longer than [`Tap::whole`]. `output` is untouched on either.
    pub fn read(&self, channel: usize, tap: Tap, output: &mut [f32]) -> Result<(), DelayError> {
        if tap.reach() > self.capacity.get() {
            return Err(DelayError::OutOfRange);
        }
        if output.len() > tap.whole {
            return Err(DelayError::Shape);
        }
        let plane = self.plane(channel)?;
        let start = self.behind(tap.whole);
        if tap.fraction == 0.0 {
            copy_wrapped(plane, start, output);
            return Ok(());
        }
        let older = self.behind(tap.whole.wrapping_add(1));
        for (offset, out) in output.iter_mut().enumerate() {
            let newer = sample(plane, start, offset);
            *out = tap
                .fraction
                .mul_add(sample(plane, older, offset) - newer, newer);
        }
        Ok(())
    }

    /// Appends `range` of every plane as the newest frames and advances the
    /// cursor by its length; a range longer than the capacity keeps its last
    /// `capacity` frames. Draining is a write of silent planes.
    ///
    /// # Errors
    /// [`DelayError::Shape`] when the plane count differs from the channel
    /// count or a plane does not cover `range`; nothing is written then.
    pub fn write<P: Deref<Target = [f32]>>(
        &mut self,
        planes: &[P],
        range: Range<usize>,
    ) -> Result<(), DelayError> {
        if planes.len() != self.channels.get()
            || planes
                .iter()
                .any(|plane| plane.get(range.clone()).is_none())
        {
            return Err(DelayError::Shape);
        }
        let capacity = self.capacity.get();
        let kept = range.len().min(capacity);
        let skipped = range.len().wrapping_sub(kept);
        let cursor = self.cursor;
        for (channel, plane) in planes.iter().enumerate() {
            let source = plane
                .get(range.start.wrapping_add(skipped)..range.end)
                .unwrap_or_default();
            let start = channel.wrapping_mul(capacity);
            if let Some(target) = self.samples.get_mut(start..start.wrapping_add(capacity)) {
                let (head, tail) = source.split_at(source.len().min(capacity.wrapping_sub(cursor)));
                if let Some(slot) = target.get_mut(cursor..cursor.wrapping_add(head.len())) {
                    slot.copy_from_slice(head);
                }
                if let Some(slot) = target.get_mut(..tail.len()) {
                    slot.copy_from_slice(tail);
                }
            }
        }
        self.cursor = wrap(cursor.wrapping_add(kept), capacity);
        Ok(())
    }

    /// Returns every frame to silence without allocating.
    pub fn reset(&mut self) {
        self.samples.fill(0.0);
        self.cursor = 0;
    }

    fn plane(&self, channel: usize) -> Result<&[f32], DelayError> {
        let capacity = self.capacity.get();
        let start = channel.checked_mul(capacity).ok_or(DelayError::Shape)?;
        self.samples
            .get(start..start.wrapping_add(capacity))
            .ok_or(DelayError::Shape)
    }

    /// Slot of the frame written `delay` frames before the next write, for
    /// `delay` in `1..=capacity + 1`.
    fn behind(&self, delay: usize) -> usize {
        let capacity = self.capacity.get();
        wrap(
            self.cursor
                .wrapping_add(capacity.wrapping_mul(2))
                .wrapping_sub(delay),
            capacity,
        )
    }
}

fn wrap(slot: usize, capacity: usize) -> usize {
    slot.checked_rem(capacity).unwrap_or(0)
}

fn sample(plane: &[f32], start: usize, offset: usize) -> f32 {
    let slot = wrap(start.wrapping_add(offset), plane.len());
    plane.get(slot).copied().unwrap_or(0.0)
}

fn copy_wrapped(plane: &[f32], start: usize, output: &mut [f32]) {
    let head = plane.get(start..).unwrap_or_default();
    let (first, second) = output.split_at_mut(output.len().min(head.len()));
    first.copy_from_slice(head.get(..first.len()).unwrap_or_default());
    second.copy_from_slice(plane.get(..second.len()).unwrap_or_default());
}
