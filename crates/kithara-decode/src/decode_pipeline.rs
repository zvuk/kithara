//! Decode pipeline with separate thread for decoding.
//!
//! Runs StreamDecoder in a blocking thread with PCM buffer channel.
//! This allows effects/resampling without impacting the audio thread.

use std::thread::JoinHandle;

use kanal::{Receiver, Sender};
use tracing::{debug, trace, warn};

use crate::{
    media_source::MediaStream,
    stream_decoder::StreamDecoder,
    types::{DecodeResult, PcmChunk},
};

/// Decode pipeline running decoder in separate thread.
///
/// # Architecture
///
/// ```text
/// MediaStream → [decode thread] → [pcm_channel] → Playback
/// ```
///
/// The PCM channel provides ~1 second of buffering (configurable),
/// ensuring smooth playback during decoder reinitialization.
///
/// # Example
///
/// ```ignore
/// let source = HlsMediaSource::new(/* ... */);
/// let stream = source.open()?;
///
/// let pipeline = DecodePipeline::new(stream, DecodePipelineConfig::default())?;
///
/// // Read PCM from channel
/// while let Ok(chunk) = pipeline.pcm_rx().recv() {
///     play_audio(chunk);
/// }
/// ```
pub struct DecodePipeline {
    /// Decode thread handle
    decode_thread: Option<JoinHandle<()>>,

    /// PCM chunk receiver
    pcm_rx: Receiver<PcmChunk<f32>>,
}

/// Configuration for decode pipeline.
#[derive(Debug, Clone)]
pub struct DecodePipelineConfig {
    /// PCM buffer size in chunks (~100ms per chunk = 10 chunks ≈ 1s)
    pub pcm_buffer_chunks: usize,
}

impl Default for DecodePipelineConfig {
    fn default() -> Self {
        Self {
            pcm_buffer_chunks: 10,
        }
    }
}

impl DecodePipeline {
    /// Create a new decode pipeline.
    ///
    /// Spawns a blocking thread for decoding.
    /// Must be called from within a Tokio runtime context.
    pub fn new(
        stream: Box<dyn MediaStream>,
        config: DecodePipelineConfig,
    ) -> DecodeResult<Self> {
        let (pcm_tx, pcm_rx) = kanal::bounded(config.pcm_buffer_chunks);

        // Capture tokio runtime handle for use in decode thread
        let rt_handle = tokio::runtime::Handle::try_current()
            .map_err(|e| crate::types::DecodeError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("no tokio runtime: {}", e),
            )))?;

        let decode_thread = std::thread::Builder::new()
            .name("decode-pipeline".to_string())
            .spawn(move || {
                // Enter tokio runtime context for async operations
                let _guard = rt_handle.enter();
                Self::decode_loop(stream, pcm_tx);
            })
            .map_err(|e| crate::types::DecodeError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("failed to spawn decode thread: {}", e),
            )))?;

        Ok(Self {
            decode_thread: Some(decode_thread),
            pcm_rx,
        })
    }

    /// Get reference to PCM receiver.
    pub fn pcm_rx(&self) -> &Receiver<PcmChunk<f32>> {
        &self.pcm_rx
    }

    /// Clone the PCM receiver (for multiple consumers).
    pub fn pcm_rx_clone(&self) -> Receiver<PcmChunk<f32>> {
        self.pcm_rx.clone()
    }

    /// Check if decode thread is still running.
    pub fn is_running(&self) -> bool {
        self.decode_thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Decode loop running in blocking thread.
    fn decode_loop(stream: Box<dyn MediaStream>, pcm_tx: Sender<PcmChunk<f32>>) {
        let decoder = match StreamDecoder::new(stream) {
            Ok(d) => d,
            Err(e) => {
                warn!(?e, "failed to create stream decoder");
                return;
            }
        };

        Self::run_decode_loop(decoder, pcm_tx);
    }

    /// Inner decode loop.
    fn run_decode_loop(mut decoder: StreamDecoder, pcm_tx: Sender<PcmChunk<f32>>) {
        let mut chunks_decoded = 0u64;
        let mut total_samples = 0u64;

        loop {
            match decoder.decode_next() {
                Ok(Some(chunk)) => {
                    if chunk.pcm.is_empty() {
                        continue;
                    }

                    chunks_decoded += 1;
                    total_samples += chunk.pcm.len() as u64;

                    if chunks_decoded % 100 == 0 {
                        trace!(
                            chunks = chunks_decoded,
                            samples = total_samples,
                            spec = ?chunk.spec,
                            "decode progress"
                        );
                    }

                    // Send chunk to channel (blocks if buffer full - backpressure)
                    if pcm_tx.send(chunk).is_err() {
                        debug!("pcm channel closed, stopping decode");
                        break;
                    }
                }
                Ok(None) => {
                    debug!(
                        chunks = chunks_decoded,
                        samples = total_samples,
                        "decode complete (EOF)"
                    );
                    break;
                }
                Err(e) => {
                    warn!(?e, "decode error, attempting to continue");
                    // Try to continue decoding
                    continue;
                }
            }
        }
    }
}

impl Drop for DecodePipeline {
    fn drop(&mut self) {
        // Close channel to signal decode thread to stop
        drop(self.pcm_rx.clone());

        // Wait for decode thread to finish
        if let Some(handle) = self.decode_thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests would go here with mock MediaStream
}
