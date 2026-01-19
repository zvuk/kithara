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

mod processor;

pub(crate) use processor::ResamplerProcessor;
