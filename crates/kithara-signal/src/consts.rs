#[cfg(test)]
use std::num::NonZeroU32;

#[cfg(test)]
pub(crate) const INTERLEAVED_RATE: NonZeroU32 =
    NonZeroU32::new(48_000).expect("48 kHz is non-zero");

pub(crate) const FAST_CHANNELS: usize = 8;

#[cfg(test)]
pub(crate) const FRAME_RATE: NonZeroU32 = NonZeroU32::new(44_100).expect("44.1 kHz is non-zero");

#[cfg(test)]
pub(crate) const BLOCK_FRAMES: usize = 480;

pub(crate) const NANOS_PER_SECOND: u128 = 1_000_000_000;
