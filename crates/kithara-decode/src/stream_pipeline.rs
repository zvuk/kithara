//! Stream-based decoding pipeline with 3-loop architecture.
//!
//! Replaces the previous 5-loop architecture with a simpler 3-loop design:
//! 1. **HLS Worker** (tokio task) - fetches segments
//! 2. **Decoder** (blocking thread) - decodes audio
//! 3. **PCM output** (channel) - streams decoded audio
//!
//! ## Architecture
//!
//! ```text
//! HlsWorkerSource (async) → [msg_channel] → StreamDecoder (blocking) → [pcm_channel]
//!        ↑                                                                     ↓
//!    [cmd_channel]                                                       [playback]
//! ```
//!
//! ## Benefits
//!
//! - **Fewer threads**: 3 loops instead of 5 (40% reduction)
//! - **Explicit boundaries**: Decoder knows when to reinitialize
//! - **Generic**: Works with any AsyncWorkerSource + StreamDecoder
//! - **Testable**: Can mock both worker and decoder

use kanal::{AsyncReceiver, AsyncSender, Receiver};
use kithara_stream::{StreamData, StreamMetadata};
use kithara_worker::{AsyncWorker, AsyncWorkerSource, Fetch, Worker};
use tokio::task::JoinHandle;
use tracing::{debug, error};

use crate::{
    stream_decoder::StreamDecoder,
    types::{DecodeResult, PcmChunk},
};

/// Stream-based decoding pipeline.
///
/// Coordinates worker (source), decoder, and buffer in a 3-loop architecture.
///
/// # Type Parameters
///
/// - `S`: Source implementing `AsyncWorkerSource` (e.g., `HlsWorkerSource`)
/// - `M`: Metadata type implementing `StreamMetadata`
/// - `D`: Data type implementing `StreamData`
///
/// # Examples
///
/// ```ignore
/// use kithara_decode::StreamPipeline;
/// use kithara_hls::HlsWorkerSource;
///
/// let source = HlsWorkerSource::new(url, options).await?;
/// let decoder = HlsStreamDecoder::new();
///
/// let pipeline = StreamPipeline::new(source, decoder).await?;
///
/// // Receive PCM chunks
/// let pcm_rx = pipeline.pcm_rx();
/// while let Ok(chunk) = pcm_rx.recv() {
///     play_audio(chunk);
/// }
/// ```
pub struct StreamPipeline<S, M, D>
where
    S: AsyncWorkerSource,
    M: StreamMetadata,
    D: StreamData,
{
    /// Worker task handle (HLS fetcher)
    worker_task: JoinHandle<()>,

    /// Decoder task handle (blocking thread)
    decoder_task: JoinHandle<()>,

    /// Command sender for worker
    cmd_tx: AsyncSender<S::Command>,

    /// PCM chunk receiver
    pcm_rx: Receiver<PcmChunk<f32>>,

    /// Phantom data for type parameters
    _phantom: std::marker::PhantomData<(M, D)>,
}

impl<S, M, D> StreamPipeline<S, M, D>
where
    S: AsyncWorkerSource,
    M: StreamMetadata,
    D: StreamData,
{
    /// Create a new stream pipeline.
    ///
    /// # Arguments
    ///
    /// - `source`: Worker source (e.g., `HlsWorkerSource`)
    /// - `decoder`: Stream decoder (e.g., `HlsStreamDecoder`)
    ///
    /// # Errors
    ///
    /// Returns error if worker or decoder initialization fails.
    pub async fn new<Dec>(source: S, decoder: Dec) -> DecodeResult<Self>
    where
        Dec: StreamDecoder<M, D, Output = PcmChunk<f32>> + Send + 'static,
        S::Chunk: Into<kithara_stream::StreamMessage<M, D>>,
    {
        // 1. Create channels
        let (cmd_tx, cmd_rx) = kanal::bounded_async(16);
        // Keep msg buffer small to limit memory usage (backpressure)
        // 1 segment = ~1-2 MB in flight (reduced from 2 for memory optimization)
        let (msg_tx, msg_rx) = kanal::bounded_async(1);
        // PCM buffer: ~400ms of audio (4 chunks at ~100ms/chunk)
        // Reduced from 10 to 4 for lower memory consumption (~140KB)
        let (pcm_tx, pcm_rx) = kanal::bounded(4);

        // 2. Create and spawn worker (HLS fetcher)
        let worker = AsyncWorker::new(source, cmd_rx, msg_tx);
        let worker_task = tokio::spawn(async move {
            worker.run().await;
        });

        // 3. Spawn decoder in blocking thread
        let decoder_task = tokio::task::spawn_blocking(move || {
            Self::decode_loop(msg_rx, decoder, pcm_tx);
        });

        Ok(Self {
            worker_task,
            decoder_task,
            cmd_tx,
            pcm_rx,
            _phantom: std::marker::PhantomData,
        })
    }

    /// Get reference to PCM receiver for reading decoded chunks.
    pub fn pcm_rx(&self) -> &Receiver<PcmChunk<f32>> {
        &self.pcm_rx
    }

    /// Get command sender for controlling the worker.
    pub fn cmd_tx(&self) -> &AsyncSender<S::Command> {
        &self.cmd_tx
    }

    /// Decode loop running in blocking thread.
    ///
    /// Receives messages from worker, decodes them incrementally, and sends
    /// PCM chunks to output. Each chunk is ~4KB (1024 samples for AAC).
    fn decode_loop<Dec>(
        msg_rx: AsyncReceiver<Fetch<S::Chunk>>,
        mut decoder: Dec,
        pcm_tx: kanal::Sender<PcmChunk<f32>>,
    ) where
        Dec: StreamDecoder<M, D, Output = PcmChunk<f32>>,
        S::Chunk: Into<kithara_stream::StreamMessage<M, D>>,
    {
        let handle = tokio::runtime::Handle::current();

        debug!("Decode loop started (incremental mode)");

        loop {
            // Receive message from worker
            let fetch = match handle.block_on(msg_rx.recv()) {
                Ok(f) => f,
                Err(_) => {
                    debug!("Message channel closed, exiting decode loop");
                    break;
                }
            };

            // Check for EOF
            if fetch.is_eof {
                debug!("EOF received, flushing decoder");

                match handle.block_on(decoder.flush()) {
                    Ok(chunks) => {
                        for chunk in chunks {
                            if pcm_tx.send(chunk).is_err() {
                                debug!("PCM receiver dropped");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!(?e, "Error flushing decoder");
                    }
                }

                break;
            }

            // Convert chunk to StreamMessage
            let message: kithara_stream::StreamMessage<M, D> = fetch.data.into();

            // Prepare message for incremental decoding
            if let Err(e) = handle.block_on(decoder.prepare_message(message)) {
                error!(?e, "Error preparing message, continuing");
                continue;
            }

            // Decode incrementally - each chunk is ~4KB
            loop {
                match handle.block_on(decoder.try_decode_chunk()) {
                    Ok(Some(pcm_chunk)) => {
                        if !pcm_chunk.pcm.is_empty() {
                            if pcm_tx.send(pcm_chunk).is_err() {
                                debug!("PCM receiver dropped, exiting decode loop");
                                return;
                            }
                        }
                    }
                    Ok(None) => {
                        // No more chunks from this message
                        break;
                    }
                    Err(e) => {
                        error!(?e, "Decode chunk error, continuing to next message");
                        break;
                    }
                }
            }
        }

        debug!("Decode loop finished");
    }

    /// Wait for pipeline to complete.
    ///
    /// Waits for both worker and decoder tasks to finish.
    pub async fn wait(self) -> DecodeResult<()> {
        // Wait for worker
        if let Err(e) = self.worker_task.await {
            error!(?e, "Worker task panicked");
        }

        // Wait for decoder
        if let Err(e) = self.decoder_task.await {
            error!(?e, "Decoder task panicked");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use bytes::Bytes;
    use kithara_stream::{StreamMessage, StreamMetadata};
    use kithara_worker::Fetch;

    use super::*;

    // Test metadata
    #[derive(Clone, Debug)]
    struct TestMetadata {
        seq: u64,
        boundary: bool,
    }

    impl StreamMetadata for TestMetadata {
        fn sequence_id(&self) -> u64 {
            self.seq
        }

        fn is_boundary(&self) -> bool {
            self.boundary
        }
    }

    // Test source
    struct TestSource {
        messages: Vec<StreamMessage<TestMetadata, Bytes>>,
        index: usize,
        epoch: u64,
    }

    #[async_trait]
    impl AsyncWorkerSource for TestSource {
        type Chunk = StreamMessage<TestMetadata, Bytes>;
        type Command = ();

        async fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
            if self.index >= self.messages.len() {
                // Return EOF with empty message
                let empty_msg = StreamMessage::new(
                    TestMetadata {
                        seq: 999,
                        boundary: false,
                    },
                    Bytes::new(),
                );
                return Fetch::new(empty_msg, true, self.epoch);
            }

            let msg = self.messages[self.index].clone();
            self.index += 1;
            Fetch::new(msg, false, self.epoch)
        }

        fn handle_command(&mut self, _cmd: Self::Command) -> u64 {
            self.epoch += 1;
            self.epoch
        }

        fn epoch(&self) -> u64 {
            self.epoch
        }
    }

    // Test decoder
    struct TestDecoder {
        decoded_count: usize,
        has_pending: bool,
    }

    #[async_trait]
    impl StreamDecoder<TestMetadata, Bytes> for TestDecoder {
        type Output = PcmChunk<f32>;

        async fn prepare_message(
            &mut self,
            _message: StreamMessage<TestMetadata, Bytes>,
        ) -> DecodeResult<()> {
            self.decoded_count += 1;
            self.has_pending = true;
            Ok(())
        }

        async fn try_decode_chunk(&mut self) -> DecodeResult<Option<Self::Output>> {
            if self.has_pending {
                self.has_pending = false;
                Ok(Some(PcmChunk::new(
                    crate::types::PcmSpec {
                        sample_rate: 44100,
                        channels: 2,
                    },
                    vec![],
                )))
            } else {
                Ok(None)
            }
        }

        async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn test_pipeline_creation() {
        let messages = vec![
            StreamMessage::new(
                TestMetadata {
                    seq: 0,
                    boundary: false,
                },
                Bytes::new(),
            ),
            StreamMessage::new(
                TestMetadata {
                    seq: 1,
                    boundary: false,
                },
                Bytes::new(),
            ),
        ];

        let source = TestSource {
            messages,
            index: 0,
            epoch: 0,
        };

        let decoder = TestDecoder {
            decoded_count: 0,
            has_pending: false,
        };

        let pipeline = StreamPipeline::new(source, decoder).await.unwrap();

        // Give it time to process
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // PCM receiver should exist
        let _pcm_rx = pipeline.pcm_rx();

        // Clean up
        drop(pipeline);
    }
}
