//! AudioSyncReader: rodio-compatible audio source adapter.
//!
//! Reads decoded audio from AudioPipeline channel and implements rodio::Source.
//!
//! This module is only available when the `rodio` feature is enabled.

use std::time::Duration;

use kanal::Receiver;
use tracing::{debug, trace};

use crate::{PcmChunk, PcmSpec};

/// rodio-compatible audio source that reads from a pipeline channel.
///
/// Implements `Iterator<Item=f32>` and `rodio::Source` traits.
pub struct AudioSyncReader {
    /// Receiver for decoded audio chunks.
    audio_rx: Receiver<PcmChunk<f32>>,
    /// Current chunk being consumed.
    current_chunk: Option<ChunkState>,
    /// Audio specification.
    spec: PcmSpec,
    /// End of stream reached.
    eof: bool,
}

/// State for current chunk being consumed.
struct ChunkState {
    samples: Vec<f32>,
    offset: usize,
}

impl AudioSyncReader {
    /// Create a new AudioSyncReader from a pipeline audio receiver.
    pub fn new(audio_rx: Receiver<PcmChunk<f32>>, spec: PcmSpec) -> Self {
        Self {
            audio_rx,
            current_chunk: None,
            spec,
            eof: false,
        }
    }

    /// Get the audio specification.
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }
}

impl Iterator for AudioSyncReader {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }

        loop {
            // Try to get sample from current chunk
            if let Some(ref mut state) = self.current_chunk {
                if state.offset < state.samples.len() {
                    let sample = state.samples[state.offset];
                    state.offset += 1;
                    return Some(sample);
                }
                // Chunk exhausted
                self.current_chunk = None;
            }

            // Need new chunk from channel
            match self.audio_rx.recv() {
                Ok(chunk) => {
                    trace!(
                        samples = chunk.samples().len(),
                        frames = chunk.frames(),
                        "AudioSyncReader received chunk"
                    );
                    self.current_chunk = Some(ChunkState {
                        samples: chunk.into_samples(),
                        offset: 0,
                    });
                }
                Err(_) => {
                    // Channel closed = end of stream
                    debug!("AudioSyncReader: channel closed, EOF");
                    self.eof = true;
                    return None;
                }
            }
        }
    }
}

impl rodio::Source for AudioSyncReader {
    fn current_span_len(&self) -> Option<usize> {
        // Return remaining samples in current chunk if available
        self.current_chunk
            .as_ref()
            .map(|state| state.samples.len() - state.offset)
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
