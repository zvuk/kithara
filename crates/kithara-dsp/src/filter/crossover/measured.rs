use std::ops::DerefMut;

use super::Crossover;

/// Planar split function for one crossover execution form.
pub type Kernel<const CHANNELS: usize, P> =
    fn(&mut Crossover<CHANNELS>, &[&[f32]; CHANNELS], &mut [[P; CHANNELS]]) -> usize;

/// Enumerates the two crossover forms and the public entry.
#[must_use]
pub const fn forms<const CHANNELS: usize, P: DerefMut<Target = [f32]>>()
-> [(&'static str, Kernel<CHANNELS, P>); 3] {
    [
        ("simd", |filter, input, bands| {
            filter.split_simd(input, bands)
        }),
        ("scalar", |filter, input, bands| {
            filter.split_scalar(input, bands)
        }),
        ("entry", |filter, input, bands| filter.split(input, bands)),
    ]
}
