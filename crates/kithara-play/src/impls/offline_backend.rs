//! Offline audio backend for testing.
//!
//! Drives the Firewheel audio graph without a real audio device.
//! Call [`OfflineBackend::render`] to manually step the graph.

#![cfg(any(test, feature = "test-utils"))]

use std::{num::NonZeroU32, time::Duration};

use firewheel::{
    StreamInfo,
    backend::{AudioBackend, BackendProcessInfo},
    node::StreamStatus,
    processor::FirewheelProcessor,
};

/// Number of stereo output channels.
const STEREO_CHANNELS: usize = 2;

/// Minimal audio backend that renders offline (no real device).
pub struct OfflineBackend {
    processor: Option<FirewheelProcessor<Self>>,
    sample_rate: u32,
    frames_rendered: u64,
}

/// Configuration for the offline backend.
#[derive(Clone)]
pub struct OfflineConfig {
    pub sample_rate: u32,
    pub block_frames: u32,
}

/// Default sample rate for offline rendering.
const DEFAULT_OFFLINE_SAMPLE_RATE: u32 = 44100;

/// Default audio block size in frames.
const DEFAULT_OFFLINE_BLOCK_FRAMES: u32 = 512;

impl Default for OfflineConfig {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_OFFLINE_SAMPLE_RATE,
            block_frames: DEFAULT_OFFLINE_BLOCK_FRAMES,
        }
    }
}

/// Errors (never happen for offline backend).
#[derive(Debug, thiserror::Error)]
#[error("offline backend error")]
pub struct OfflineError;

impl AudioBackend for OfflineBackend {
    type Config = OfflineConfig;
    type Enumerator = ();
    type Instant = std::time::Instant;
    type StartStreamError = OfflineError;
    type StreamError = OfflineError;

    fn enumerator() -> Self::Enumerator {}

    fn start_stream(config: Self::Config) -> Result<(Self, StreamInfo), Self::StartStreamError> {
        let sr = NonZeroU32::new(config.sample_rate).expect("non-zero sample rate");
        let block = NonZeroU32::new(config.block_frames).expect("non-zero block frames");
        let declick = NonZeroU32::new(DEFAULT_OFFLINE_BLOCK_FRAMES).expect("non-zero");
        let stream_info = StreamInfo {
            sample_rate: sr,
            sample_rate_recip: 1.0 / f64::from(config.sample_rate),
            prev_sample_rate: sr,
            max_block_frames: block,
            num_stream_in_channels: 0,
            #[expect(clippy::cast_possible_truncation)]
            num_stream_out_channels: STEREO_CHANNELS as u32,
            input_to_output_latency_seconds: 0.0,
            declick_frames: declick,
            output_device_id: String::from("offline"),
            input_device_id: None,
        };
        let backend = Self {
            processor: None,
            sample_rate: config.sample_rate,
            frames_rendered: 0,
        };
        Ok((backend, stream_info))
    }

    fn set_processor(&mut self, processor: FirewheelProcessor<Self>) {
        self.processor = Some(processor);
    }

    fn poll_status(&mut self) -> Result<(), Self::StreamError> {
        Ok(())
    }

    fn delay_from_last_process(&self, _process_timestamp: Self::Instant) -> Option<Duration> {
        None
    }
}

impl OfflineBackend {
    /// Render `frames` of audio. Calls `process_interleaved` on the
    /// firewheel processor, driving all audio nodes in the graph.
    ///
    /// Returns the rendered stereo output buffer (interleaved LRLR).
    pub fn render(&mut self, frames: usize) -> Vec<f32> {
        let channels = STEREO_CHANNELS;
        let total_samples = frames * channels;
        let input = vec![0.0f32; 0]; // no input channels
        let mut output = vec![0.0f32; total_samples];

        if let Some(ref mut processor) = self.processor {
            let info = BackendProcessInfo {
                num_in_channels: 0,
                num_out_channels: channels,
                frames,
                process_timestamp: std::time::Instant::now(),
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "frame counter precision loss acceptable for test timing"
                )]
                duration_since_stream_start: Duration::from_secs_f64(
                    self.frames_rendered as f64 / f64::from(self.sample_rate),
                ),
                input_stream_status: StreamStatus::empty(),
                output_stream_status: StreamStatus::empty(),
                dropped_frames: 0,
            };
            processor.process_interleaved(&input, &mut output, info);
        }

        self.frames_rendered += frames as u64;
        output
    }
}
