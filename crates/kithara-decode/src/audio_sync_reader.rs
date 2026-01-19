//! AudioSyncReader: rodio-compatible audio source adapter.
//!
//! Reads decoded audio from Pipeline buffer and implements rodio::Source.
//!
//! This module is only available when the `rodio` feature is enabled.

use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
use ringbuf::traits::Consumer;
use tracing::trace;

use crate::{PcmBuffer, PcmSpec};

/// rodio-compatible audio source that reads from a pipeline buffer.
///
/// Implements `Iterator<Item=f32>` and `rodio::Source` traits.
pub struct AudioSyncReader {
    /// Ring buffer consumer for reading samples.
    consumer: Arc<Mutex<ringbuf::HeapCons<f32>>>,
    /// Shared PCM buffer for metadata.
    buffer: Arc<PcmBuffer>,
    /// Audio specification.
    spec: PcmSpec,
    /// End of stream reached.
    eof: bool,
    /// Internal read buffer.
    read_buffer: Vec<f32>,
    /// Current position in read buffer.
    buffer_offset: usize,
    /// Samples available in read buffer.
    buffer_samples: usize,
}

impl AudioSyncReader {
    /// Create a new AudioSyncReader from a pipeline consumer.
    ///
    /// # Arguments
    /// - `consumer`: Ring buffer consumer from Pipeline
    /// - `buffer`: Shared PCM buffer for metadata
    /// - `spec`: Audio specification (should match buffer spec)
    pub fn new(
        consumer: Arc<Mutex<ringbuf::HeapCons<f32>>>,
        buffer: Arc<PcmBuffer>,
        spec: PcmSpec,
    ) -> Self {
        const BUFFER_SIZE: usize = 8192; // Samples, not frames
        Self {
            consumer,
            buffer,
            spec,
            eof: false,
            read_buffer: vec![0.0; BUFFER_SIZE],
            buffer_offset: 0,
            buffer_samples: 0,
        }
    }

    /// Get the audio specification.
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Read more samples from ring buffer into internal buffer.
    fn fill_buffer(&mut self) -> bool {
        if self.eof {
            return false;
        }

        // Wait for data with polling
        let mut attempts = 0;
        const MAX_ATTEMPTS: u32 = 1000;
        loop {
            let mut consumer = self.consumer.lock();
            let n = consumer.pop_slice(&mut self.read_buffer);
            drop(consumer);

            if n > 0 {
                self.buffer_samples = n;
                self.buffer_offset = 0;
                return true;
            }

            // Check if EOF
            if self.buffer.is_eof() {
                self.eof = true;
                return false;
            }

            // Wait a bit for more data
            attempts += 1;
            if attempts > MAX_ATTEMPTS {
                // Timeout
                self.eof = true;
                return false;
            }
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
    }
}

impl Iterator for AudioSyncReader {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }

        // Try to get sample from current buffer
        if self.buffer_offset < self.buffer_samples {
            let sample = self.read_buffer[self.buffer_offset];
            self.buffer_offset += 1;
            return Some(sample);
        }

        // Buffer exhausted, need more data
        if self.fill_buffer() {
            // Successfully filled buffer
            if self.buffer_offset < self.buffer_samples {
                let sample = self.read_buffer[self.buffer_offset];
                self.buffer_offset += 1;
                return Some(sample);
            }
        }

        // EOF or timeout
        trace!("AudioSyncReader: EOF");
        None
    }
}

impl rodio::Source for AudioSyncReader {
    fn current_span_len(&self) -> Option<usize> {
        // Return remaining samples in internal buffer
        if self.buffer_samples > self.buffer_offset {
            Some(self.buffer_samples - self.buffer_offset)
        } else {
            None
        }
    }

    fn channels(&self) -> u16 {
        self.spec.channels
    }

    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        // Duration is unknown for streaming sources
        None
    }
}
