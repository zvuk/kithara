pub(super) use crate::consts::DEFAULT_DECLICK;

/// Frames in one source chunk of a test lane.
pub(super) const LANE_CHUNK_FRAMES: u32 = 4096;

/// Widest level change, in dB, of a tone whose keylock engine renders on.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub(super) const TONE_LEVEL_DB: f32 = 0.1;
