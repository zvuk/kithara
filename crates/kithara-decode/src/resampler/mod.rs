//! Audio resampling module for sample rate conversion and speed control.
//!
//! Built on [rubato](https://crates.io/crates/rubato) for high-quality resampling.
//!
//! ## Features
//!
//! - Sample rate conversion (e.g., 48kHz → 44.1kHz)
//! - Playback speed control (0.5x - 2.0x)
//! - Automatic passthrough when no resampling needed
//! - Lock-free speed updates
//!
//! ## Usage
//!
//! ```ignore
//! let mut decoder = AudioPipeline::open(source).await?;
//! let decoder_rx = decoder.take_audio_receiver().unwrap();
//!
//! let mut resampler = ResamplerPipeline::new(
//!     decoder_rx,
//!     decoder.spec(),
//!     44100, // target sample rate
//! );
//!
//! let audio_rx = resampler.take_audio_receiver().unwrap();
//! let audio_source = AudioSyncReader::new(audio_rx, resampler.output_spec());
//!
//! // Speed control (lock-free)
//! resampler.set_speed(1.5);
//! ```

mod pipeline;
mod processor;

pub use pipeline::{ResamplerCommand, ResamplerPipeline};
