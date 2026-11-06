use std::{
    num::NonZeroUsize,
    ops::{DerefMut, Range},
};

use fearless_simd::{Level, dispatch, prelude::*};

use crate::filter::FilterError;

mod consts {
    /// A section that passes its input through.
    pub(super) const IDENTITY: [f32; 5] = [1.0, 0.0, 0.0, 0.0, 0.0];
    /// Section memory below this on every entry restarts from zero at the
    /// end of a call, before its free decay reaches subnormals.
    pub(super) const FLUSH_FLOOR: f32 = 1.0e-30;
}

/// Direct form I sections run on every channel, `S::f32s::LEN` channels per
/// vector. Channel `c`, section `s` keeps `[x1, x2, y1, y2]` at
/// `state[c × sections + s]`. A call ends by zeroing every section memory
/// that has fallen below `1e-30`, so a silent channel beside a playing one
/// stops ringing before subnormals.
pub(crate) struct Cascade {
    channels: NonZeroUsize,
    coefficients: Box<[[f32; 5]]>,
    state: Box<[[f32; 4]]>,
}

impl Cascade {
    pub(crate) fn new(channels: NonZeroUsize, sections: NonZeroUsize) -> Result<Self, FilterError> {
        let states = channels
            .get()
            .checked_mul(sections.get())
            .ok_or(FilterError::Shape)?;
        Ok(Self {
            channels,
            coefficients: vec![consts::IDENTITY; sections.get()].into_boxed_slice(),
            state: vec![[0.0; 4]; states].into_boxed_slice(),
        })
    }

    pub(crate) fn set_section(
        &mut self,
        section: usize,
        coefficients: [f32; 5],
    ) -> Result<(), FilterError> {
        let slot = self
            .coefficients
            .get_mut(section)
            .ok_or(FilterError::Shape)?;
        *slot = coefficients;
        Ok(())
    }

    pub(crate) fn process<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
    ) -> Result<(), FilterError> {
        if planes.len() != self.channels.get()
            || planes
                .iter()
                .any(|plane| plane.get(range.clone()).is_none())
        {
            return Err(FilterError::Shape);
        }
        dispatch!(Level::new(), simd => cascade_kernel(simd, self, planes, range));
        Ok(())
    }

    pub(crate) fn reset(&mut self) {
        self.state.fill([0.0; 4]);
    }

    pub(crate) fn copy_state_from(&mut self, source: &Self) -> Result<(), FilterError> {
        if self.channels != source.channels || self.state.len() != source.state.len() {
            return Err(FilterError::Shape);
        }
        self.state.copy_from_slice(&source.state);
        Ok(())
    }
}

/// Runs every section over `range` of each plane in place; the caller has
/// checked the shape.
#[inline(always)]
pub(super) fn cascade_kernel<S: Simd, P: DerefMut<Target = [f32]>>(
    simd: S,
    cascade: &mut Cascade,
    planes: &mut [P],
    range: Range<usize>,
) {
    let Cascade {
        coefficients,
        state,
        ..
    } = cascade;
    let lanes = S::f32s::LEN;
    let sections = coefficients.len();
    let zero = S::f32s::splat(simd, 0.0);
    for (group, states) in planes
        .chunks_mut(lanes)
        .zip(state.chunks_mut(lanes.saturating_mul(sections)))
    {
        for (section, &[b0, b1, b2, a1, a2]) in coefficients.iter().enumerate() {
            let [b0, b1, b2, na1, na2] =
                [b0, b1, b2, -a1, -a2].map(|value| S::f32s::splat(simd, value));
            let [mut x1, mut x2, mut y1, mut y2] = [zero; 4];
            for (lane, channel) in states.chunks(sections).enumerate() {
                let Some(delay) = channel.get(section) else {
                    continue;
                };
                for (vector, value) in [&mut x1, &mut x2, &mut y1, &mut y2].into_iter().zip(delay) {
                    if let Some(slot) = vector.as_mut_slice().get_mut(lane) {
                        *slot = *value;
                    }
                }
            }
            for frame in range.clone() {
                let mut x = zero;
                for (slot, plane) in x.as_mut_slice().iter_mut().zip(group.iter()) {
                    *slot = plane.get(frame).copied().unwrap_or(0.0);
                }
                let y = x.mul_add(
                    b0,
                    x1.mul_add(b1, x2.mul_add(b2, y1.mul_add(na1, y2.mul_add(na2, zero)))),
                );
                (x2, x1, y2, y1) = (x1, x, y1, y);
                for (plane, sample) in group.iter_mut().zip(y.as_slice()) {
                    if let Some(slot) = plane.get_mut(frame) {
                        *slot = *sample;
                    }
                }
            }
            for (lane, channel) in states.chunks_mut(sections).enumerate() {
                if let Some(delay) = channel.get_mut(section) {
                    let memory = [x1, x2, y1, y2]
                        .map(|vector| vector.as_slice().get(lane).copied().unwrap_or(0.0));
                    let ringing = memory
                        .iter()
                        .any(|value| value.abs() >= consts::FLUSH_FLOOR);
                    *delay = if ringing { memory } else { [0.0; 4] };
                }
            }
        }
    }
}
