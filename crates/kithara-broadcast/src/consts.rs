use std::num::{NonZeroU32, NonZeroUsize};

#[cfg(all(not(target_arch = "wasm32"), test))]
use kithara_platform::time::Duration;

#[cfg(test)]
pub(crate) const ADTS_PAYLOAD: usize = 100;

pub(crate) const BUFFER_FRAMES: NonZeroUsize = match NonZeroUsize::new(96_000) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const FAIRNESS_YIELD_INTERVAL: NonZeroU32 = match NonZeroU32::new(16) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const GENERATION_CAPACITY: NonZeroUsize = match NonZeroUsize::new(8) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const TICK_FRAMES: NonZeroUsize = match NonZeroUsize::new(4_096) {
    Some(value) => value,
    None => unreachable!(),
};

#[cfg(test)]
pub(crate) const MPEG_TIMESCALE: u32 = 90_000;

#[cfg(test)]
pub(crate) const OWNER: &[u8] = b"com.apple.streaming.transportStreamTimestamp\0";

#[cfg(test)]
pub(crate) const ID3_SAMPLE_RATE: u32 = 48_000;

#[cfg(test)]
pub(crate) const WRAP: u64 = 1 << 33;

#[cfg(test)]
pub(crate) const SEGMENT_PAYLOAD: usize = 200;

#[cfg(test)]
pub(crate) const UNITS_PER_TARGET: usize = 188;

#[cfg(test)]
pub(crate) const UNIT_DURATION: u32 = 1_024;

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
pub(crate) const AMPLITUDE: f32 = 0.25;

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
pub(crate) const BROADCAST_SAMPLE_RATE: usize = 48_000;

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
pub(crate) const TARGET: Duration = Duration::from_millis(500);

#[cfg(test)]
pub(crate) const DURATION_TS: u32 = 192_512;

#[cfg(test)]
pub(crate) const TIMESCALE: u32 = 48_000;
