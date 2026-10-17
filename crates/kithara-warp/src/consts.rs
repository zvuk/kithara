use std::num::NonZeroUsize;

pub(crate) const MODEL_SECONDS_PER_MINUTE: f64 = 60.0;

#[cfg(test)]
pub(crate) const BPM: f64 = 120.0;

#[cfg(test)]
pub(crate) const RATES: [u32; 3] = [44_100, 48_000, 96_000];

#[cfg(test)]
pub(crate) const SAMPLE_RATE: u32 = 48_000;

#[cfg(test)]
pub(crate) const SECONDS_PER_BEAT: f64 = 0.5;

#[cfg(test)]
pub(crate) const BEATS: i64 = 400;

#[cfg(test)]
pub(crate) const HOST_BPM: f64 = 100.0;

#[cfg(test)]
pub(crate) const QUEUE_TEMPOS: [f64; 5] = [124.0, 96.0, 132.0, 74.0, 140.0];

#[cfg(test)]
pub(crate) const PROJECTION_SECONDS_PER_MINUTE: f64 = 60.0;

#[cfg(test)]
pub(crate) const AFTER_EOF_FRAME: f64 = 48_000.5;

#[cfg(test)]
pub(crate) const EOF_FRAME: f64 = 48_000.0;

#[cfg(test)]
pub(crate) const FRAME_COUNT: u64 = 48_000;

#[cfg(test)]
pub(crate) const BLOCK_FRAMES: usize = 480;

pub(crate) const NATIVE_PACKET_FRAMES: usize = 480;
pub(crate) const MAX_NATIVE_STRETCH: f64 = 2.0;

pub(crate) const DEFAULT_SOURCE_BLOCK_FRAMES: NonZeroUsize = match NonZeroUsize::new(8192) {
    Some(frames) => frames,
    None => unreachable!(),
};

#[cfg(feature = "render")]
#[cfg(test)]
pub(crate) const CH: u16 = 2;

#[cfg(feature = "render")]
#[cfg(test)]
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
pub(crate) const F0: f64 = 440.0;

/// FFT length for the pitch (dominant-frequency) check.
#[cfg(feature = "render")]
#[cfg(test)]
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
pub(crate) const N: usize = 1 << 14;

#[cfg(feature = "render")]
#[cfg(test)]
pub(crate) const SR: u32 = 44_100;

pub(crate) const SOURCE_RADIUS: usize = 16;
