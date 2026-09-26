use std::num::{NonZeroU32, NonZeroUsize};

pub(crate) const BUFFER_FRAMES: NonZeroUsize = match NonZeroUsize::new(96_000) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const DISPATCHER_CAPACITY: NonZeroUsize = NonZeroUsize::MIN;

pub(crate) const FAIRNESS_YIELD_INTERVAL: NonZeroU32 = match NonZeroU32::new(16) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const GENERATION_CAPACITY: NonZeroUsize = match NonZeroUsize::new(8) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const TICK_FRAMES: NonZeroUsize = match NonZeroUsize::new(1_024) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const CHANNELS: u16 = 2;
pub(crate) const NO_CUT: u64 = u64::MAX;
pub(crate) const STEREO: usize = 2;
