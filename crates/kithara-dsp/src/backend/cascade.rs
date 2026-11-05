use std::{
    num::NonZeroUsize,
    ops::{DerefMut, Mul, Range},
};

use fearless_simd::{Level, dispatch, prelude::*};

use crate::filter::FilterError;

mod consts {
    /// A section that passes its input through.
    pub(super) const IDENTITY: [f32; 5] = [1.0, 0.0, 0.0, 0.0, 0.0];
    /// An output sample below this is written as zero, and memory below it
    /// is cleared at every flush point.
    pub(super) const FLUSH_FLOOR: f32 = 1.0e-30;
    /// Frames of the stream between two flush points.
    pub(super) const FLUSH_PERIOD: usize = 64;
}

/// Direct form I sections run on every channel, `S::f32s::LEN` channels per
/// vector. Channel `c`, section `s` keeps `[x1, x2, y1, y2]` at
/// `state[c × sections + s]`. An output sample below `1e-30` is written as
/// zero, and every 64 frames of the stream the memory below `1e-30` is
/// cleared, so a silent channel beside a playing one falls silent without
/// the flush lengthening the recursion. Both rules count frames of the
/// stream, so the result never depends on call boundaries.
pub(crate) struct Cascade {
    channels: NonZeroUsize,
    coefficients: Box<[[f32; 5]]>,
    state: Box<[[f32; 4]]>,
    /// Frames run since the last flush point.
    phase: usize,
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
            phase: 0,
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
        self.check(planes, &range)?;
        self.run(planes, range);
        Ok(())
    }

    /// [`FilterError::Shape`] unless `planes` holds one plane per channel and
    /// each covers `range`.
    pub(crate) fn check<P: DerefMut<Target = [f32]>>(
        &self,
        planes: &[P],
        range: &Range<usize>,
    ) -> Result<(), FilterError> {
        if planes.len() != self.channels.get()
            || planes
                .iter()
                .any(|plane| plane.get(range.clone()).is_none())
        {
            return Err(FilterError::Shape);
        }
        Ok(())
    }

    /// Filters a `range` that [`Self::check`] admitted.
    pub(crate) fn run<P: DerefMut<Target = [f32]>>(&mut self, planes: &mut [P], range: Range<usize>) {
        let level = Level::new();
        let mut start = range.start;
        while start < range.end {
            let until = consts::FLUSH_PERIOD.saturating_sub(self.phase);
            let end = range.end.min(start.saturating_add(until));
            dispatch!(level, simd => cascade_kernel(simd, self, planes, start..end));
            self.phase = self.phase.saturating_add(end.saturating_sub(start));
            if self.phase >= consts::FLUSH_PERIOD {
                self.phase = 0;
                self.flush();
            }
            start = end;
        }
    }

    /// Clears memory below the flush floor.
    fn flush(&mut self) {
        for value in self.state.iter_mut().flatten() {
            if value.abs() < consts::FLUSH_FLOOR {
                *value = 0.0;
            }
        }
    }

    pub(crate) fn reset(&mut self) {
        self.state.fill([0.0; 4]);
    }

    pub(crate) fn copy_state_from(&mut self, source: &Self) -> Result<(), FilterError> {
        if self.channels != source.channels || self.state.len() != source.state.len() {
            return Err(FilterError::Shape);
        }
        self.state.copy_from_slice(&source.state);
        self.phase = source.phase;
        Ok(())
    }
}

/// Runs every section over `range` of each plane in place; the caller has
/// checked the shape. A pass carries up to four sections in registers, so
/// each frame runs through all of them before the next one starts.
#[inline(always)]
pub(super) fn cascade_kernel<S: Simd, P: DerefMut<Target = [f32]>>(
    simd: S,
    cascade: &mut Cascade,
    planes: &mut [P],
    range: Range<usize>,
) {
    type One<S> = Section<S>;
    type Two<S> = (Section<S>, Section<S>);
    let Cascade {
        coefficients,
        state,
        ..
    } = cascade;
    let lanes = S::f32s::LEN;
    let sections = coefficients.len();
    for (group, states) in planes
        .chunks_mut(lanes)
        .zip(state.chunks_mut(lanes.saturating_mul(sections)))
    {
        let mut first = 0;
        while first < sections {
            let block = Block {
                simd,
                coefficients,
                sections,
                first,
                group: &mut *group,
                states: &mut *states,
                range: range.clone(),
            };
            first = first.saturating_add(match sections.saturating_sub(first) {
                1 => pass::<S, P, One<S>>(block),
                2 => pass::<S, P, Two<S>>(block),
                3 => pass::<S, P, (One<S>, Two<S>)>(block),
                _ => pass::<S, P, (Two<S>, Two<S>)>(block),
            });
        }
    }
}

/// One group of channels and the sections from `first` that a pass runs.
struct Block<'a, S, P> {
    simd: S,
    coefficients: &'a [[f32; 5]],
    sections: usize,
    first: usize,
    group: &'a mut [P],
    states: &'a mut [[f32; 4]],
    range: Range<usize>,
}

/// Runs the sections of `T` over `block`; returns how many it ran.
#[inline(always)]
fn pass<S: Simd, P: DerefMut<Target = [f32]>, T: Stages<S>>(block: Block<'_, S, P>) -> usize {
    let Block {
        simd,
        coefficients,
        sections,
        first,
        group,
        states,
        range,
    } = block;
    let zero = S::f32s::splat(simd, 0.0);
    let floor = S::f32s::splat(simd, consts::FLUSH_FLOOR);
    let mut stages = T::load(simd, coefficients, states, sections, first);
    for frame in range {
        let mut x = zero;
        for (slot, plane) in x.as_mut_slice().iter_mut().zip(group.iter()) {
            *slot = plane.get(frame).copied().unwrap_or(0.0);
        }
        let y = stages.tick(x);
        let y = y.abs().simd_ge(floor).select(y, zero);
        for (plane, sample) in group.iter_mut().zip(y.as_slice()) {
            if let Some(slot) = plane.get_mut(frame) {
                *slot = *sample;
            }
        }
    }
    stages.store(states, sections, first);
    T::COUNT
}

/// Sections a pass keeps in registers, chained in order.
trait Stages<S: Simd>: Sized {
    const COUNT: usize;

    /// Section `first` and the ones after it, per lane from `states`.
    fn load(
        simd: S,
        coefficients: &[[f32; 5]],
        states: &[[f32; 4]],
        sections: usize,
        first: usize,
    ) -> Self;

    /// Filters one frame of every lane through each section.
    fn tick(&mut self, x: S::f32s) -> S::f32s;

    /// Writes the memory back to `states`.
    fn store(&self, states: &mut [[f32; 4]], sections: usize, first: usize);
}

/// Taps `[b0, b1, b2, −a1, −a2]` and memory `[x1, x2, y1, y2]` of one
/// section across the lanes of a vector.
struct Section<S: Simd> {
    taps: [S::f32s; 5],
    memory: [S::f32s; 4],
}

impl<S: Simd> Stages<S> for Section<S> {
    const COUNT: usize = 1;

    #[inline(always)]
    fn load(
        simd: S,
        coefficients: &[[f32; 5]],
        states: &[[f32; 4]],
        sections: usize,
        first: usize,
    ) -> Self {
        let [b0, b1, b2, a1, a2] = coefficients
            .get(first)
            .copied()
            .unwrap_or(consts::IDENTITY);
        let memory = std::array::from_fn(|term| {
            S::f32s::from_fn(simd, |lane| {
                states
                    .chunks(sections)
                    .nth(lane)
                    .and_then(|channel| channel.get(first))
                    .and_then(|values| values.get(term))
                    .copied()
                    .unwrap_or(0.0)
            })
        });
        Self {
            taps: [b0, b1, b2, -a1, -a2].map(|value| S::f32s::splat(simd, value)),
            memory,
        }
    }

    /// The feed-forward terms stay off the recursion: a frame waits on one
    /// fused multiply-add of the previous output.
    #[inline(always)]
    fn tick(&mut self, x: S::f32s) -> S::f32s {
        let [b0, b1, b2, na1, na2] = self.taps;
        let [x1, x2, y1, y2] = self.memory;
        let forward = x.mul_add(b0, x1.mul_add(b1, Mul::mul(x2, b2)));
        let y = y1.mul_add(na1, y2.mul_add(na2, forward));
        self.memory = [x, x1, y, y1];
        y
    }

    #[inline(always)]
    fn store(&self, states: &mut [[f32; 4]], sections: usize, first: usize) {
        for (lane, channel) in states.chunks_mut(sections).enumerate() {
            if let Some(values) = channel.get_mut(first) {
                *values = self
                    .memory
                    .map(|vector| vector.as_slice().get(lane).copied().unwrap_or(0.0));
            }
        }
    }
}

impl<S: Simd, A: Stages<S>, B: Stages<S>> Stages<S> for (A, B) {
    const COUNT: usize = A::COUNT.saturating_add(B::COUNT);

    #[inline(always)]
    fn load(
        simd: S,
        coefficients: &[[f32; 5]],
        states: &[[f32; 4]],
        sections: usize,
        first: usize,
    ) -> Self {
        (
            A::load(simd, coefficients, states, sections, first),
            B::load(
                simd,
                coefficients,
                states,
                sections,
                first.saturating_add(A::COUNT),
            ),
        )
    }

    #[inline(always)]
    fn tick(&mut self, x: S::f32s) -> S::f32s {
        let y = self.0.tick(x);
        self.1.tick(y)
    }

    #[inline(always)]
    fn store(&self, states: &mut [[f32; 4]], sections: usize, first: usize) {
        self.0.store(states, sections, first);
        self.1.store(states, sections, first.saturating_add(A::COUNT));
    }
}
