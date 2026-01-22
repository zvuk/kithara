//! Generic stream decoder implementation using Decoder trait.
//!
//! Provides a StreamDecoder implementation that:
//! - Works with any StreamMetadata (not HLS-specific)
//! - Uses Decoder trait for actual decoding (Symphonia, mock, etc.)
//! - Handles boundaries by recreating decoder from media_info()
//! - No knowledge of HLS, init segments, or transport specifics

use async_trait::async_trait;
use kithara_stream::{MediaInfo, StreamData, StreamMessage, StreamMetadata};
use tracing::debug;

use crate::{
    chunked_reader::SharedChunkedReader,
    decoder::Decoder,
    stream_decoder::StreamDecoder,
    symphonia_mod::SymphoniaDecoder,
    types::{DecodeError, DecodeResult, PcmChunk, PcmSpec},
};

/// Generic stream decoder that uses Decoder trait.
///
/// Handles any stream format where messages contain full data (not fragments).
/// On boundary (variant switch, codec change), creates new decoder using media_info().
///
/// # Type Parameters
/// - `M`: Metadata type implementing StreamMetadata
/// - `D`: Data type implementing StreamData (usually Bytes)
///
/// # Examples
///
/// ```ignore
/// use kithara_decode::GenericStreamDecoder;
/// use kithara_stream::StreamMessage;
///
/// let decoder = GenericStreamDecoder::new();
///
/// for message in stream_messages {
///     let pcm = decoder.decode_message(message).await?;
///     play_audio(pcm);
/// }
/// ```
pub struct GenericStreamDecoder<M, D>
where
    M: StreamMetadata,
    D: StreamData<Item = u8>,
{
    /// Shared chunked reader for continuous data stream
    chunked_reader: SharedChunkedReader,

    /// Current decoder instance (recreated only on codec change)
    decoder: Option<Box<dyn Decoder + Send + Sync>>,

    /// Current PCM spec (sample rate, channels)
    current_spec: Option<PcmSpec>,

    /// Total messages processed (for debugging)
    messages_processed: usize,

    /// Total boundaries encountered
    boundaries_encountered: usize,

    /// Phantom data for generics
    _phantom: std::marker::PhantomData<(M, D)>,
}

impl<M, D> GenericStreamDecoder<M, D>
where
    M: StreamMetadata,
    D: StreamData<Item = u8>,
{
    /// Create a new generic stream decoder.
    pub fn new() -> Self {
        Self {
            chunked_reader: SharedChunkedReader::new(),
            decoder: None,
            current_spec: None,
            messages_processed: 0,
            boundaries_encountered: 0,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Create decoder from chunked reader and media info.
    fn create_decoder(&mut self, media_info: &MediaInfo) -> DecodeResult<()> {
        debug!(
            ?media_info,
            "creating decoder from shared chunked reader"
        );

        // Clone the reader (Arc clone, cheap) and pass to decoder
        // Decoder owns the clone, we keep our reference for appending data
        let reader_clone = self.chunked_reader.clone();
        let decoder = SymphoniaDecoder::new_from_media_info(reader_clone, media_info, true)?;
        self.current_spec = Some(decoder.spec());
        self.decoder = Some(Box::new(decoder));

        debug!(spec = ?self.current_spec, "decoder created successfully");

        Ok(())
    }

    /// Decode all chunks from current decoder.
    fn decode_all_chunks(&mut self) -> DecodeResult<PcmChunk<f32>> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| DecodeError::DecodeError("No decoder available".to_string()))?;

        let mut all_samples = Vec::new();
        let mut spec = decoder.spec();
        let mut chunks_decoded = 0;

        loop {
            match decoder.next_chunk() {
                Ok(Some(chunk)) => {
                    spec = chunk.spec;
                    all_samples.extend_from_slice(&chunk.pcm);
                    chunks_decoded += 1;
                }
                Ok(None) => {
                    debug!(chunks_decoded, "decoder EOF reached");
                    break;
                }
                Err(e) => {
                    debug!(?e, chunks_decoded, "decode ended");
                    break;
                }
            }
        }

        self.current_spec = Some(spec);

        debug!(
            chunks_decoded,
            samples = all_samples.len(),
            "decoded all chunks"
        );

        Ok(PcmChunk::new(spec, all_samples))
    }
}

impl<M, D> Default for GenericStreamDecoder<M, D>
where
    M: StreamMetadata,
    D: StreamData<Item = u8>,
{
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<M> StreamDecoder<M, bytes::Bytes> for GenericStreamDecoder<M, bytes::Bytes>
where
    M: StreamMetadata,
{
    type Output = PcmChunk<f32>;

    async fn decode_message(&mut self, message: StreamMessage<M, bytes::Bytes>) -> DecodeResult<Self::Output> {
        let (meta, data) = message.into_parts();

        self.messages_processed += 1;

        // Log memory usage periodically
        if self.messages_processed % 10 == 0 {
            debug!(
                messages_processed = self.messages_processed,
                boundaries = self.boundaries_encountered,
                data_size = data.len(),
                "GenericStreamDecoder stats"
            );
        }

        // Skip empty messages early
        if data.is_empty() {
            // Still track boundaries even for empty messages
            if meta.is_boundary() {
                self.boundaries_encountered += 1;
            }
            return Ok(PcmChunk::new(
                self.current_spec.unwrap_or(PcmSpec {
                    sample_rate: 44100,
                    channels: 2,
                }),
                vec![],
            ));
        }

        // Handle boundary (codec/format change - decoder must be recreated)
        if meta.is_boundary() {
            self.boundaries_encountered += 1;

            debug!(
                sequence_id = meta.sequence_id(),
                boundaries_total = self.boundaries_encountered,
                "boundary detected - recreating decoder"
            );

            // Clear old data and add new
            self.chunked_reader.clear();
            self.chunked_reader.append(data);

            // Get media info and create new decoder
            let media_info = meta.media_info().ok_or_else(|| {
                DecodeError::DecodeError("No media info on boundary".to_string())
            })?;

            self.create_decoder(&media_info)?;
        } else {
            // No boundary - just append data to reader, decoder continues
            debug!(
                sequence_id = meta.sequence_id(),
                data_len = data.len(),
                "appending data to existing decoder"
            );

            self.chunked_reader.append(data);

            // If no decoder yet, create one (first message)
            if self.decoder.is_none() {
                let media_info = meta.media_info().ok_or_else(|| {
                    DecodeError::DecodeError("No media info for initial decoder".to_string())
                })?;

                self.create_decoder(&media_info)?;
            }
        }

        // Decode from decoder (same or newly created)
        let pcm = self.decode_all_chunks()?;

        // Periodically release consumed memory
        self.chunked_reader.release_consumed();

        Ok(pcm)
    }

    async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
        debug!(
            messages_processed = self.messages_processed,
            boundaries = self.boundaries_encountered,
            "flushing decoder"
        );

        // Clear decoder
        self.decoder = None;

        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};

    #[derive(Debug, Clone)]
    struct TestMetadata {
        seq: u64,
        boundary: bool,
        codec: Option<AudioCodec>,
        container: Option<ContainerFormat>,
    }

    impl StreamMetadata for TestMetadata {
        fn sequence_id(&self) -> u64 {
            self.seq
        }

        fn is_boundary(&self) -> bool {
            self.boundary
        }

        fn media_info(&self) -> Option<MediaInfo> {
            if self.codec.is_some() || self.container.is_some() {
                Some(MediaInfo {
                    codec: self.codec,
                    container: self.container,
                    sample_rate: None,
                    channels: None,
                })
            } else {
                None
            }
        }
    }

    #[tokio::test]
    async fn test_decoder_creation() {
        let decoder = GenericStreamDecoder::<TestMetadata, Bytes>::new();
        assert_eq!(decoder.messages_processed, 0);
        assert_eq!(decoder.boundaries_encountered, 0);
    }

    #[tokio::test]
    async fn test_empty_message_handling() {
        let mut decoder = GenericStreamDecoder::<TestMetadata, Bytes>::new();

        let meta = TestMetadata {
            seq: 1,
            boundary: false,
            codec: None,
            container: None,
        };

        let msg = StreamMessage::new(meta, Bytes::new());
        let result = decoder.decode_message(msg).await;

        // Empty messages return empty PcmChunk, not error
        assert!(result.is_ok());
        let pcm = result.unwrap();
        assert_eq!(pcm.pcm.len(), 0);
    }

    #[tokio::test]
    async fn test_boundary_counter() {
        let mut decoder = GenericStreamDecoder::<TestMetadata, Bytes>::new();

        // Boundary with empty data
        let meta1 = TestMetadata {
            seq: 1,
            boundary: true,
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Fmp4),
        };
        let msg1 = StreamMessage::new(meta1, Bytes::new());
        let _ = decoder.decode_message(msg1).await;

        assert_eq!(decoder.boundaries_encountered, 1);

        // Another boundary
        let meta2 = TestMetadata {
            seq: 2,
            boundary: true,
            codec: Some(AudioCodec::Mp3),
            container: None,
        };
        let msg2 = StreamMessage::new(meta2, Bytes::new());
        let _ = decoder.decode_message(msg2).await;

        assert_eq!(decoder.boundaries_encountered, 2);
    }

    #[tokio::test]
    async fn test_flush() {
        let mut decoder = GenericStreamDecoder::<TestMetadata, Bytes>::new();

        // Process a message (even empty)
        let meta = TestMetadata {
            seq: 1,
            boundary: false,
            codec: None,
            container: None,
        };
        let msg = StreamMessage::new(meta, Bytes::new());
        let _ = decoder.decode_message(msg).await;

        // Flush
        let remaining = decoder.flush().await.unwrap();
        assert_eq!(remaining.len(), 0);

        // Decoder should be cleared
        assert!(decoder.decoder.is_none());
    }
}
