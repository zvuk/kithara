/// Frames a mono source must fill in one planar read, and the source
/// frames it may consume doing so. A mono chunk carries one sample per
/// frame, so filling `MONO_OUTPUT_FRAMES` output frames must consume
/// exactly that many source frames.
#[cfg(test)]
pub(crate) const MONO_OUTPUT_FRAMES: usize = 4;

pub(crate) const AUDIO_EVENT_CAPACITY: usize = 64;
pub(crate) const PROGRESS_EMIT_MIN_DELTA_MS: u64 = 100;

/// The AAC decoder's post-seek onset transient outlasts 20 ms; 40 ms keeps that measured
/// transition inside the existing linear generation join.
pub(crate) const JOIN_MICROS: u32 = 40_000;

pub(crate) const MICROS_PER_SEC: u32 = 1_000_000;
pub(crate) const MIN_JOIN_FRAMES: u16 = 2;

/// Output ring depth. wasm needs a deeper ring because its worker is
/// scheduled coarsely.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const AUDIO_BUFFER_CHUNKS: usize = 10;

#[cfg(target_arch = "wasm32")]
pub(crate) const AUDIO_BUFFER_CHUNKS: usize = 32;

/// Chunks buffered before preload readiness is signalled.
pub(crate) const PRELOAD_CHUNKS: usize = 3;

pub(crate) const DEFAULT_READ_AHEAD_BYTES: u64 = 32 * 1024;
pub(crate) const PRIME_STEPS_PER_PASS: usize = 8;
pub(crate) const ANCHOR_RESOLUTION: &str = "seek anchor resolution failed";
pub(crate) const NANOS_PER_SEC: u128 = 1_000_000_000;

#[cfg(test)]
pub(crate) const REBUILD_CHANNELS: u16 = 2;

#[cfg(test)]
pub(crate) const ROUTE_CHUNK_FRAMES: usize = 256;

#[cfg(test)]
pub(crate) const ROUTE_SAMPLE_RATE: u32 = 48_000;

#[cfg(test)]
pub(crate) const SAMPLE_RATE: u32 = 44_100;

#[cfg(test)]
pub(crate) const CAPTURE_END_SEGMENT: usize = 6;

#[cfg(test)]
pub(crate) const SPLICE_CHANNELS: usize = 2;

#[cfg(test)]
pub(crate) const SLQ_VARIANT: usize = 0;

#[cfg(test)]
pub(crate) const SMQ_VARIANT: usize = 1;

#[cfg(test)]
pub(crate) const SPLICE_SEGMENT: u32 = 3;

#[cfg(test)]
pub(crate) const TOTAL_SEGMENTS: usize = 7;
