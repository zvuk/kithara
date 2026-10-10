use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};

use kithara_platform::time::Duration;

pub(crate) const SESSION_PUMP_INTERVAL: Duration = Duration::from_millis(10);
pub(crate) const TEMPO_SMOOTH_SECONDS: f64 = 0.005;

pub(crate) const DEFAULT_SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
    Some(sample_rate) => sample_rate,
    None => unreachable!(),
};

#[cfg(test)]
pub(crate) const GRAPH_BLOCK_FRAMES: usize = 128;

#[cfg(test)]
pub(crate) const STEREO_CHANNELS: usize = 2;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const RING_ADMISSION_SAMPLE_RATE: u32 = 48_000;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const RING_ADMISSION_BLOCK_FRAMES: u32 = 512;

/// The thread a test session that holds probe decks runs on.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const DECK_SESSION: &str = "host-deck-session";

#[cfg(test)]
pub(crate) const TRANSPORT_BLOCK_FRAMES: usize = 480;

#[cfg(test)]
pub(crate) const TRANSPORT_SAMPLE_RATE: u32 = 48_000;

pub(crate) const MAX_DECKS: NonZeroU16 = match NonZeroU16::new(8) {
    Some(value) => value,
    None => unreachable!(),
};
pub(crate) const DECK_CAPACITY: NonZeroUsize = match NonZeroUsize::new(32) {
    Some(value) => value,
    None => unreachable!(),
};
