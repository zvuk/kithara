//! AudioSyncReader: rodio-compatible audio source adapter.
//!
//! Reads decoded audio from a channel and implements rodio::Source.

use std::time::Duration;

use kithara_bufpool::PcmBuf;
use tracing::{debug, trace};

use crate::{PcmChunk, PcmSpec};

/// rodio-compatible audio source that reads from a channel.
///
/// Implements `Iterator<Item=f32>` and `rodio::Source` traits.
/// Spec is updated dynamically from received chunks (handles variant switches).
pub struct AudioSyncReader {
    /// Channel receiver for reading PCM chunks.
    pcm_rx: kanal::Receiver<PcmChunk>,
    /// Current audio specification (updated from chunks).
    spec: PcmSpec,
    /// End of stream reached.
    eof: bool,
    /// Current chunk being read (auto-recycles to pool on drop).
    current_chunk: Option<PcmBuf>,
    /// Current position in chunk.
    chunk_offset: usize,
}

impl AudioSyncReader {
    /// Create a new AudioSyncReader from a PCM chunk channel.
    ///
    /// # Arguments
    /// - `pcm_rx`: Channel receiver for PCM chunks
    /// - `initial_spec`: Initial audio specification (updated from chunks)
    pub fn new(pcm_rx: kanal::Receiver<PcmChunk>, initial_spec: PcmSpec) -> Self {
        Self {
            pcm_rx,
            spec: initial_spec,
            eof: false,
            current_chunk: None,
            chunk_offset: 0,
        }
    }

    /// Get the current audio specification.
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
        match self.pcm_rx.recv() {
            Ok(chunk) => {
                trace!(
                    samples = chunk.pcm.len(),
                    spec = ?chunk.spec,
                    "AudioSyncReader: received chunk"
                );
                // Update spec from chunk (handles dynamic format changes)
                self.spec = chunk.spec;
                // Old current_chunk auto-recycles via Drop on reassignment
                self.current_chunk = Some(chunk.pcm);
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
        if let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.len()
        {
            let sample = chunk[self.chunk_offset];
            self.chunk_offset += 1;
            return Some(sample);
        }

        // Chunk exhausted or no chunk - need more data
        if self.fill_buffer()
            && let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.len()
        {
            let sample = chunk[self.chunk_offset];
            self.chunk_offset += 1;
            return Some(sample);
        }

        // EOF
        trace!("AudioSyncReader: EOF");
        None
    }
}

impl ::rodio::Source for AudioSyncReader {
    fn current_span_len(&self) -> Option<usize> {
        // Return remaining samples in current chunk
        if let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.len()
        {
            return Some(chunk.len() - self.chunk_offset);
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
