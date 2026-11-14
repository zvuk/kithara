use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};

use kithara_platform::time::Duration;

pub(crate) const SESSION_PUMP_INTERVAL: Duration = Duration::from_millis(10);

/// Web output is partitioned into the existing 128-frame graph processing quantum.
#[cfg(target_arch = "wasm32")]
pub(crate) const WEB_RENDER_QUANTUM: usize = 128;
/// Stereo interleaved storage for one web processing quantum.
#[cfg(target_arch = "wasm32")]
pub(crate) const WEB_OUTPUT_SAMPLES: usize = 256;

/// Firewheel's existing processing limit when the device chooses its callback size.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const SESSION_OUTPUT_MAX_BLOCK_FRAMES: NonZeroU32 =
    NonZeroU32::new(1024).expect("nonzero graph block limit");

/// Fixed conversion scratch holds at least one frame for every nonzero CPAL u16 channel count.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const SESSION_OUTPUT_CONVERSION_SAMPLES: usize = 65_536;

pub(crate) const DEFAULT_SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
    Some(sample_rate) => sample_rate,
    None => unreachable!(),
};

/// Highest output rate a session runs at, the Web Audio limit: the output chain takes its
/// lookahead storage for this rate once, so a stream restart never takes more.
pub(crate) const MAX_SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(768_000) {
    Some(sample_rate) => sample_rate,
    None => unreachable!(),
};

/// Nodes of the session output chain: master, limiter, metronome and inbox return.
pub(crate) const SESSION_OUTPUT_NODES: usize = 4;

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
