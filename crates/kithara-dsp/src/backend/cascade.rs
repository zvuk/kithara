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
    /// end of each frame, before its free decay reaches subnormals.
    pub(super) const FLUSH_FLOOR: f32 = 1.0e-30;
}

/// Direct form I sections over channel planes. Memory retirement counts
/// stream frames, independently of caller chunk boundaries.
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
    pub(crate) fn run<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
    ) {
        dispatch!(Level::new(), simd => cascade_kernel(simd, self, planes, range));
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
/// checked the shape. A pass carries up to four sections in registers, so
/// each frame runs through all of them before the next one starts.
#[inline(always)]
pub(super) fn cascade_kernel<S: Simd, P: DerefMut<Target = [f32]>>(
    simd: S,
    cascade: &mut Cascade,
    planes: &mut [P],
    range: Range<usize>,
) {
    let sections = cascade.coefficients.len();
    for (group, states) in planes.chunks_mut(crate::consts::LANES).zip(
        cascade
            .state
            .chunks_mut(crate::consts::LANES.saturating_mul(sections)),
    ) {
        let mut borrowed = std::array::from_fn::<_, { crate::consts::LANES }, _>(|_| &mut [][..]);
        let channels = group.len();
        for (reference, plane) in borrowed.iter_mut().zip(group) {
            *reference = &mut **plane;
        }
        group_kernel(
            simd,
            &cascade.coefficients,
            states,
            &mut borrowed[..channels],
            range.clone(),
        );
    }
}

/// The hot core takes borrowed planes, so callers do not instantiate it at
/// their own optimization level. No sample copy or allocation crosses it.
#[inline(never)]
fn group_kernel<S: Simd>(
    simd: S,
    coefficients: &[[f32; 5]],
    states: &mut [[f32; 4]],
    planes: &mut [&mut [f32]],
    range: Range<usize>,
) {
    type One<S> = Section<S>;
    type Two<S> = (Section<S>, Section<S>);
    let sections = coefficients.len();
    for (group, states) in planes
        .chunks_mut(S::f32s::LEN)
        .zip(states.chunks_mut(S::f32s::LEN.saturating_mul(sections)))
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
                1 => pass::<S, One<S>>(block),
                2 => pass::<S, Two<S>>(block),
                3 => pass::<S, (One<S>, Two<S>)>(block),
                _ => pass::<S, (Two<S>, Two<S>)>(block),
            });
        }
    }
}

struct Block<'a, 'pcm, S> {
    simd: S,
    coefficients: &'a [[f32; 5]],
    sections: usize,
    first: usize,
    group: &'a mut [&'pcm mut [f32]],
    states: &'a mut [[f32; 4]],
    range: Range<usize>,
}

/// Carries section memory in registers across frames.
#[inline(always)]
fn pass<S: Simd, T: Stages<S>>(block: Block<'_, '_, S>) -> usize {
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
    let mut stages = T::load(simd, coefficients, states, sections, first);
    for frame in range {
        let mut x = zero;
        for (slot, plane) in x.as_mut_slice().iter_mut().zip(group.iter()) {
            *slot = plane.get(frame).copied().unwrap_or(0.0);
        }
        let y = stages.tick(x);
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
    simd: S,
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
        let [b0, b1, b2, a1, a2] = coefficients.get(first).copied().unwrap_or(consts::IDENTITY);
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
            simd,
        }
    }

    #[inline(always)]
    fn tick(&mut self, x: S::f32s) -> S::f32s {
        let [b0, b1, b2, na1, na2] = self.taps;
        let [x1, x2, y1, y2] = self.memory;
        let zero = S::f32s::splat(self.simd, 0.0);
        let floor = S::f32s::splat(self.simd, consts::FLUSH_FLOOR);
        let y = x.mul_add(
            b0,
            x1.mul_add(b1, x2.mul_add(b2, y1.mul_add(na1, y2.mul_add(na2, zero)))),
        );
        let ringing = x.abs().simd_ge(floor)
            | x1.abs().simd_ge(floor)
            | y.abs().simd_ge(floor)
            | y1.abs().simd_ge(floor);
        self.memory = [x, x1, y, y1].map(|value| ringing.select(value, zero));
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
        self.1
            .store(states, sections, first.saturating_add(A::COUNT));
    }
}
