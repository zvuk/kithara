use std::num::{NonZeroU32, NonZeroUsize};

#[cfg(test)]
use kithara_events::TrackId;
use kithara_platform::time::Duration;

use crate::rt::PlayerNodeProcessor;

pub(crate) const SLOT_TRACKS: usize = PlayerNodeProcessor::MAX_TRACKS;

#[cfg(test)]
pub(crate) const BACKGROUND: TrackId = TrackId(9);

#[cfg(test)]
pub(crate) const OUTGOING: TrackId = TrackId(7);

#[cfg(test)]
pub(crate) const PROMOTED: TrackId = TrackId(8);

pub(crate) const DISCRIMINATOR_DOMAIN: &[u8] = b"kithara.play.query-discriminator.v1\0";
pub(crate) const HASH_BYTES: usize = 16;
pub(crate) const IDENTITY_DOMAIN: &[u8] = b"kithara.play.query-identity.v1\0";

#[cfg(test)]
pub(crate) const BLOCK_FRAMES: usize = 512;

#[cfg(test)]
pub(crate) const SAMPLE_RATE: u32 = 44_100;

pub(crate) const ACTIVE_WAIT_TIMEOUT: Duration = Duration::from_millis(1);
pub(crate) const BACKPRESSURE_POLL_INTERVAL: Duration = Duration::from_micros(250);

pub(crate) const CAPACITY: NonZeroUsize = match NonZeroUsize::new(16) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const FAIRNESS_YIELD_INTERVAL: NonZeroU32 = match NonZeroU32::new(16) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const TASK_BURST: NonZeroU32 = match NonZeroU32::new(32) {
    Some(value) => value,
    None => unreachable!(),
};

/// EWMA weight for per-chunk samples (≈ last ~10 chunks dominate).
pub(crate) const LOAD_ALPHA: f32 = 0.2;

pub(crate) const MS_PER_SEC: f64 = 1000.0;
