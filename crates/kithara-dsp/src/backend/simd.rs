use std::{
    num::NonZeroUsize,
    ops::{Add, Deref, Div, Mul, Range, Shr, Sub},
};

use fearless_simd::{Level, dispatch, prelude::*};
use num_traits::AsPrimitive;

pub(super) use super::{last_loud::kernel as last_loud_kernel, load::padded};

/// Replaces `NaN`, ±infinity, subnormals and −0.0 with `+0.0` in place; every
/// other value keeps its bits.
pub fn sanitize(samples: &mut [f32]) {
    dispatch!(Level::new(), simd => sanitize_kernel(simd, samples));
}

#[inline(always)]
pub(super) fn sanitize_kernel<S: Simd>(simd: S, samples: &mut [f32]) {
    let smallest = S::f32s::splat(simd, f32::MIN_POSITIVE);
    let largest = S::f32s::splat(simd, f32::MAX);
    let zero = S::f32s::splat(simd, 0.0);
    update(simd, samples, [], |x, []| {
        sanitized::<S>(x, smallest, largest, zero)
    });
}

/// `samples[i] /= divisor`, correctly rounded, over the whole slice; returns
/// its length.
pub(crate) fn div(samples: &mut [f32], divisor: f32) -> usize {
    dispatch!(Level::new(), simd => div_kernel(simd, samples, divisor))
}

#[inline(always)]
pub(super) fn div_kernel<S: Simd>(simd: S, samples: &mut [f32], divisor: f32) -> usize {
    let divisor = S::f32s::splat(simd, divisor);
    update(simd, samples, [], |x, []| x.div(divisor))
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
#[inline(always)]
pub(crate) fn scale_in_place(samples: &mut [f32], gain: f32) -> usize {
    let level = Level::new();
    // The target already supplies NEON, without a feature trampoline.
    if let Some(neon) = level.as_neon() {
        return scale_in_place_kernel(neon, samples, gain);
    }
    dispatch!(level, simd => scale_in_place_kernel(simd, samples, gain))
}

#[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
#[inline(never)]
pub(crate) fn scale_in_place(samples: &mut [f32], gain: f32) -> usize {
    dispatch!(Level::new(), simd => scale_in_place_kernel(simd, samples, gain))
}

#[inline(always)]
pub(super) fn scale_in_place_kernel<S: Simd>(simd: S, samples: &mut [f32], gain: f32) -> usize {
    let len = samples.len();
    let gains = S::f32s::splat(simd, gain);
    let mut chunks = samples.chunks_exact_mut(S::f32s::LEN);
    for chunk in &mut chunks {
        S::f32s::from_slice(simd, chunk)
            .mul(gains)
            .store_slice(chunk);
    }
    for sample in chunks.into_remainder() {
        *sample *= gain;
    }
    len
}

/// `output[i] = start + i*step`, rounded once while `i` stays below 2^24;
/// returns `output.len()`.
pub(crate) fn ramp(output: &mut [f32], start: f32, step: f32) -> usize {
    dispatch!(Level::new(), simd => ramp_kernel(simd, output, start, step))
}

#[inline(always)]
pub(super) fn ramp_kernel<S: Simd>(simd: S, output: &mut [f32], start: f32, step: f32) -> usize {
    let mut index = S::f32s::splat(simd, 0.0);
    let mut count = 0.0_f32;
    for slot in index.as_mut_slice() {
        *slot = count;
        count += 1.0;
    }
    let stride = S::f32s::splat(simd, count);
    let (start, step) = (S::f32s::splat(simd, start), S::f32s::splat(simd, step));
    update(simd, output, [], |_, []| {
        let value = index.mul_add_precise(step, start);
        index = index.add(stride);
        value
    })
}

/// `acc[i] += x[i]*y[i]`, rounded once, over the common prefix of the three
/// slices; returns its length.
pub(crate) fn mul_add(x: &[f32], y: &[f32], acc: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => mul_add_kernel(simd, x, y, acc))
}

#[inline(always)]
pub(super) fn mul_add_kernel<S: Simd>(simd: S, x: &[f32], y: &[f32], acc: &mut [f32]) -> usize {
    update(simd, acc, [x, y], |acc, [x, y]| x.mul_add_precise(y, acc))
}

/// `target[i] = f(target[i], [inputs[0][i], ...])` over the common prefix of
/// the target and the inputs, a vector at a time; returns the prefix length.
#[inline(always)]
pub(super) fn update<S: Simd, I: Inputs<S>>(
    simd: S,
    target: &mut [f32],
    inputs: I,
    mut f: impl FnMut(S::f32s, I::Vectors) -> S::f32s,
) -> usize {
    let len = inputs.bound(target.len());
    blocks::<S>(len, |start, end| {
        if let Some(block) = target.get_mut(start..end) {
            store::<S>(f(vector(simd, block), inputs.load(simd, start, end)), block);
        }
    });
    len
}

/// The slices an element-wise kernel reads beside its target. Each arity
/// destructures its array, so a block loads its vectors without a loop over
/// the inputs, which the size profile would neither unroll nor inline.
pub(super) trait Inputs<S: Simd>: Copy {
    /// One vector per input.
    type Vectors;
    /// `len` capped at every input's length.
    fn bound(self, len: usize) -> usize;
    /// The vectors of `start..end`, zero-padded to full lanes.
    fn load(self, simd: S, start: usize, end: usize) -> Self::Vectors;
}

impl<S: Simd> Inputs<S> for [&[f32]; 0] {
    type Vectors = [S::f32s; 0];

    #[inline(always)]
    fn bound(self, len: usize) -> usize {
        len
    }

    #[inline(always)]
    fn load(self, _: S, _: usize, _: usize) -> Self::Vectors {
        []
    }
}

impl<S: Simd> Inputs<S> for [&[f32]; 1] {
    type Vectors = [S::f32s; 1];

    #[inline(always)]
    fn bound(self, len: usize) -> usize {
        let [x] = self;
        len.min(x.len())
    }

    #[inline(always)]
    fn load(self, simd: S, start: usize, end: usize) -> Self::Vectors {
        let [x] = self;
        [vector(simd, x.get(start..end).unwrap_or_default())]
    }
}

impl<S: Simd> Inputs<S> for [&[f32]; 2] {
    type Vectors = [S::f32s; 2];

    #[inline(always)]
    fn bound(self, len: usize) -> usize {
        let [x, y] = self;
        len.min(x.len()).min(y.len())
    }

    #[inline(always)]
    fn load(self, simd: S, start: usize, end: usize) -> Self::Vectors {
        let [x, y] = self;
        [
            vector(simd, x.get(start..end).unwrap_or_default()),
            vector(simd, y.get(start..end).unwrap_or_default()),
        ]
    }
}

/// Calls `f(start, end)` for each vector-wide block of `0..len`, the last one
/// shorter when `len` is not a multiple of the lane count. An index loop:
/// the size profile leaves slice splitting and chunk iterators as calls.
#[inline(always)]
pub(super) fn blocks<S: Simd>(len: usize, mut f: impl FnMut(usize, usize)) {
    let mut start = 0;
    while start < len {
        let end = len.min(start.saturating_add(S::f32s::LEN));
        f(start, end);
        start = end;
    }
}

/// The vector holding `block`, zero-padded when it is shorter than the lanes.
#[inline(always)]
pub(super) fn vector<S: Simd>(simd: S, block: &[f32]) -> S::f32s {
    if block.len() == S::f32s::LEN {
        S::f32s::from_slice(simd, block)
    } else {
        padded(simd, block)
    }
}

/// Writes the first `block.len()` lanes of `value` into `block`.
#[inline(always)]
pub(super) fn store<S: Simd>(value: S::f32s, block: &mut [f32]) {
    if block.len() == S::f32s::LEN {
        value.store_slice(block);
    } else {
        for (slot, lane) in block.iter_mut().zip(value.as_slice()) {
            *slot = *lane;
        }
    }
}

/// `x` where `MIN_POSITIVE <= |x| <= MAX`, `+0.0` elsewhere; `NaN` fails both tests.
#[inline(always)]
fn sanitized<S: Simd>(x: S::f32s, smallest: S::f32s, largest: S::f32s, zero: S::f32s) -> S::f32s {
    let magnitude = x.abs();
    let normal = magnitude.simd_ge(smallest).select(x, zero);
    magnitude.simd_le(largest).select(normal, zero)
}

pub(crate) fn crossfade_linear(
    outgoing: &[f32],
    samples: &mut [f32],
    channels: NonZeroUsize,
    first: usize,
    frames: usize,
) -> usize {
    dispatch!(Level::new(), simd => crossfade_linear_kernel(simd, outgoing, samples, channels, first, frames))
}

#[inline(always)]
pub(super) fn crossfade_linear_kernel<S: Simd>(
    simd: S,
    outgoing: &[f32],
    samples: &mut [f32],
    channels: NonZeroUsize,
    first: usize,
    frames: usize,
) -> usize {
    let count = outgoing.len().min(samples.len()) / channels;
    let len = count.wrapping_mul(channels.get());
    let (Some(outgoing), Some(samples)) = (outgoing.get(..len), samples.get_mut(..len)) else {
        return 0;
    };
    let maximum: usize = u32::MAX.as_();
    let frame_limit = maximum.wrapping_sub(S::f32s::LEN.wrapping_mul(2));
    // Reserve padded lanes and the final advance without widening the common path.
    if first <= frame_limit
        && count <= frame_limit.wrapping_sub(first)
        && channels.get() <= maximum.wrapping_sub(S::f32s::LEN)
    {
        crossfade_linear_u32(simd, outgoing, samples, channels, first.as_(), frames);
        return count;
    }
    crossfade_linear_frames(simd, outgoing, samples, channels, first.as_(), frames);
    count
}

#[inline(always)]
fn crossfade_linear_frames<S: Simd>(
    simd: S,
    outgoing: &[f32],
    samples: &mut [f32],
    channels: NonZeroUsize,
    first: u128,
    frames: usize,
) {
    let mut frame = first;
    let mut channel = channels.get();
    let denominator: f32 = frames.as_();
    let mut gain = 0.0_f32;
    crossfade_linear_gains(simd, outgoing, samples, || {
        let mut gains = S::f32s::splat(simd, 0.0);
        for lane in gains.as_mut_slice() {
            if channel == channels.get() {
                // Exact narrowing avoids wide float conversion for ordinary frame positions.
                let position: f32 = if frame <= u32::MAX.as_() {
                    let frame: u32 = frame.as_();
                    frame.as_()
                } else {
                    frame.as_()
                };
                gain = position.div(denominator);
                // Slice bounds keep the frame counter below u128::MAX.
                frame = frame.wrapping_add(1);
                channel = 0;
            }
            *lane = gain;
            channel = channel.wrapping_add(1);
        }
        gains
    });
}

#[inline(always)]
fn crossfade_linear_u32<S: Simd>(
    simd: S,
    outgoing: &[f32],
    samples: &mut [f32],
    channels: NonZeroUsize,
    first: u32,
    frames: usize,
) {
    if channels.get() > S::f32s::LEN {
        crossfade_linear_channels(simd, outgoing, samples, channels.get().as_(), first, frames);
        return;
    }
    if !channels.get().is_power_of_two() || S::f32s::LEN & channels.get().wrapping_sub(1) != 0 {
        crossfade_linear_frames(simd, outgoing, samples, channels, first.as_(), frames);
        return;
    }
    let shift = channels.get().trailing_zeros();
    let mut positions = crossfade_lane_offsets::<S>(simd)
        .shr(shift)
        .add(S::u32s::splat(simd, first));
    let step = S::u32s::splat(simd, S::f32s::LEN.wrapping_shr(shift).as_());
    let denominator = S::f32s::splat(simd, frames.as_());
    crossfade_linear_gains(simd, outgoing, samples, || {
        let gain = positions.to_float::<S::f32s>().div(denominator);
        positions = positions.add(step);
        gain
    });
}

/// A vector spans at most two frames when a frame is wider than the vector.
#[inline(always)]
fn crossfade_linear_channels<S: Simd>(
    simd: S,
    outgoing: &[f32],
    samples: &mut [f32],
    channels: u32,
    first: u32,
    frames: usize,
) {
    let offsets = crossfade_lane_offsets::<S>(simd);
    let denominator: f32 = frames.as_();
    let position: f32 = first.as_();
    let mut current_gain = position.div(denominator);
    let mut next_frame = first.wrapping_add(1);
    let position: f32 = next_frame.as_();
    let mut next_gain = position.div(denominator);
    let width: u32 = S::f32s::LEN.as_();
    let mut channel = 0_u32;
    crossfade_linear_gains(simd, outgoing, samples, || {
        let boundary = S::u32s::splat(simd, channels.wrapping_sub(channel));
        let gain = offsets.simd_ge(boundary).select(
            S::f32s::splat(simd, next_gain),
            S::f32s::splat(simd, current_gain),
        );
        channel = channel.wrapping_add(width);
        if channel >= channels {
            channel = channel.wrapping_sub(channels);
            current_gain = next_gain;
            next_frame = next_frame.wrapping_add(1);
            let position: f32 = next_frame.as_();
            next_gain = position.div(denominator);
        }
        gain
    });
}

#[inline(always)]
fn crossfade_lane_offsets<S: Simd>(simd: S) -> S::u32s {
    let mut offsets = S::u32s::splat(simd, 0);
    for (index, offset) in offsets.as_mut_slice().iter_mut().enumerate() {
        *offset = index.as_();
    }
    offsets
}

#[inline(always)]
fn crossfade_linear_gains<S: Simd>(
    simd: S,
    outgoing: &[f32],
    samples: &mut [f32],
    mut gains: impl FnMut() -> S::f32s,
) {
    let one = S::f32s::splat(simd, 1.0);
    let lanes = S::f32s::LEN;
    let len = samples.len();
    let full = samples.chunks_exact(lanes).len().wrapping_mul(lanes);
    let mut start = 0;
    while start < len {
        let gain = gains();
        if start < full {
            let end = start.wrapping_add(lanes);
            if let (Some(outgoing), Some(block)) =
                (outgoing.get(start..end), samples.get_mut(start..end))
            {
                crossfade_linear_blend::<S>(
                    S::f32s::from_slice(simd, outgoing),
                    S::f32s::from_slice(simd, block),
                    gain,
                    one,
                )
                .store_slice(block);
            }
            start = end;
        } else {
            let (Some(outgoing), Some(tail)) = (outgoing.get(start..), samples.get_mut(start..))
            else {
                break;
            };
            store::<S>(
                crossfade_linear_blend::<S>(padded(simd, outgoing), padded(simd, tail), gain, one),
                tail,
            );
            break;
        }
    }
}

#[inline(always)]
fn crossfade_linear_blend<S: Simd>(
    outgoing: S::f32s,
    incoming: S::f32s,
    gain: S::f32s,
    one: S::f32s,
) -> S::f32s {
    let blended = outgoing.mul_add_precise(one.sub(gain), incoming.mul(gain));
    outgoing
        .bitcast::<S::u32s>()
        .simd_eq(incoming.bitcast::<S::u32s>())
        .select(incoming, blended)
}

/// The first frame of `range` where every plane holds `|x| <= limit`, or its
/// end. The caller has checked that every plane covers the entire range.
pub(crate) fn first_quiet<P: Deref<Target = [f32]>>(
    planes: &[P],
    range: Range<usize>,
    limit: f32,
) -> usize {
    dispatch!(Level::new(), simd => first_quiet_kernel(simd, planes, range, limit))
}

#[inline(always)]
pub(super) fn first_quiet_kernel<S: Simd, P: Deref<Target = [f32]>>(
    simd: S,
    planes: &[P],
    range: Range<usize>,
    limit: f32,
) -> usize {
    let bound = S::f32s::splat(simd, limit);
    let mut frame = range.start;
    while let Some(end) = frame
        .checked_add(S::f32s::LEN)
        .filter(|end| *end <= range.end)
    {
        let mut quiet = u64::MAX;
        for plane in planes {
            let block = plane.get(frame..end).expect("validated plane range");
            quiet &= S::f32s::from_slice(simd, block)
                .abs()
                .simd_le(bound)
                .to_bitmask();
            if quiet == 0 {
                break;
            }
        }
        if quiet != 0 {
            let lane = usize::try_from(quiet.trailing_zeros()).expect("SIMD lane fits usize");
            return frame.saturating_add(lane);
        }
        frame = end;
    }
    (frame..range.end)
        .find(|frame| {
            planes
                .iter()
                .all(|plane| plane.get(*frame).expect("validated frame").abs() <= limit)
        })
        .unwrap_or(range.end)
}

pub(crate) fn last_loud(samples: &[f32], limit: f32) -> Option<usize> {
    dispatch!(Level::new(), simd => last_loud_kernel(simd, samples, limit))
}
