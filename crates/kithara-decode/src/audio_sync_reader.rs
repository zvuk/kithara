//! AudioSyncReader: rodio-compatible audio source adapter.
//!
//! Reads decoded audio from a channel and implements rodio::Source.
//!
//! This module is only available when the `rodio` feature is enabled.

use std::time::Duration;

use tracing::{debug, trace};

use crate::PcmSpec;

/// rodio-compatible audio source that reads from a channel.
///
/// Implements `Iterator<Item=f32>` and `rodio::Source` traits.
pub struct AudioSyncReader {
    /// Channel receiver for reading sample chunks.
    sample_rx: kanal::Receiver<Vec<f32>>,
    /// Audio specification.
    spec: PcmSpec,
    /// End of stream reached.
    eof: bool,
    /// Current chunk being read.
    current_chunk: Option<Vec<f32>>,
    /// Current position in chunk.
    chunk_offset: usize,
}

impl AudioSyncReader {
    /// Create a new AudioSyncReader from a sample channel.
    ///
    /// # Arguments
    /// - `sample_rx`: Channel receiver for PCM sample chunks
    /// - `spec`: Audio specification
    pub fn new(sample_rx: kanal::Receiver<Vec<f32>>, spec: PcmSpec) -> Self {
        Self {
            sample_rx,
            spec,
            eof: false,
            current_chunk: None,
            chunk_offset: 0,
        }
    }

    /// Get the audio specification.
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Receive next chunk from channel.
    ///
    /// Blocks until data is available or channel is closed.
    fn fill_buffer(&mut self) -> bool {
        if self.eof {
            return false;
        }

        // Blocking receive from channel
        match self.sample_rx.recv() {
            Ok(chunk) => {
                debug!(samples = chunk.len(), "AudioSyncReader: received chunk");
                self.current_chunk = Some(chunk);
                self.chunk_offset = 0;
                true
            }
            Err(_) => {
                // Channel closed (EOF)
                debug!("AudioSyncReader: channel closed (EOF)");
                self.eof = true;
                false
            }
        }
    }
}

impl Iterator for AudioSyncReader {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }

        // Try to get sample from current chunk
        if let Some(ref chunk) = self.current_chunk {
            if self.chunk_offset < chunk.len() {
                let sample = chunk[self.chunk_offset];
                self.chunk_offset += 1;
                return Some(sample);
            }
        }

        // Chunk exhausted or no chunk - need more data
        if self.fill_buffer() {
            // Successfully received new chunk
            if let Some(ref chunk) = self.current_chunk {
                if self.chunk_offset < chunk.len() {
                    let sample = chunk[self.chunk_offset];
                    self.chunk_offset += 1;
                    return Some(sample);
                }
            }
        }

        // EOF
        trace!("AudioSyncReader: EOF");
        None
    }
}

impl rodio::Source for AudioSyncReader {
    fn current_span_len(&self) -> Option<usize> {
        // Return remaining samples in current chunk
        if let Some(ref chunk) = self.current_chunk {
            if self.chunk_offset < chunk.len() {
                return Some(chunk.len() - self.chunk_offset);
            }
        }
        None
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
