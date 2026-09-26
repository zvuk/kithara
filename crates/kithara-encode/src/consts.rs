#[cfg(all(
    test,
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
use crate::StreamBackend;
#[cfg(all(
    test,
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
use crate::StreamEncoder;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
#[cfg(test)]
pub(crate) const STREAM_BIT_RATE: u64 = 128_000;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
#[cfg(test)]
pub(crate) const STREAM_CHANNELS: u16 = 2;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
#[cfg(test)]
pub(crate) const FRAMES: usize = 4_096;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
#[cfg(test)]
pub(crate) const STREAM_SAMPLE_RATE: u32 = 48_000;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
#[cfg(test)]
#[cfg(feature = "ffmpeg")]
pub(crate) const FFMPEG_PRIMING_FRAMES: usize = StreamEncoder::FRAME_SAMPLES;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
#[cfg(test)]
#[cfg(feature = "ffmpeg")]
pub(crate) const FFMPEG_BACKEND: StreamBackend = StreamBackend::Ffmpeg;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
#[cfg(test)]
#[cfg(feature = "fdk-aac")]
pub(crate) const FDK_PRIMING_FRAMES: usize = 2 * StreamEncoder::FRAME_SAMPLES;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
#[cfg(test)]
#[cfg(feature = "fdk-aac")]
pub(crate) const FDK_BACKEND: StreamBackend = StreamBackend::Fdk;

#[cfg(feature = "ffmpeg")]
#[cfg(test)]
pub(crate) const I16_SCALE: f32 = 32_768.0;

#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
#[cfg(test)]
pub(crate) const AAC_BIT_RATE: u64 = 128_000;

#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
#[cfg(test)]
pub(crate) const AAC_CHANNELS: u16 = 2;

#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
#[cfg(test)]
pub(crate) const ENCODER_DELAY: u32 = 2_112;

#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
#[cfg(test)]
pub(crate) const AAC_SAMPLE_RATE: u32 = 48_000;

#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
#[cfg(test)]
pub(crate) const TRAILING_DELAY: u32 = 1_920;
