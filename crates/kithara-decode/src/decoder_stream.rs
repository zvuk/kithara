//! Generic stream decoder implementation using Decoder trait.
//!
//! Provides a StreamDecoder implementation that:
//! - Works with any StreamMetadata (not HLS-specific)
//! - Uses Decoder trait for actual decoding (Symphonia, mock, etc.)
//! - Handles boundaries by recreating decoder from media_info()
//! - Decodes incrementally (~4KB chunks) for memory efficiency

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

/// Generic stream decoder with incremental decoding.
///
/// Decodes audio incrementally (~4KB per chunk) instead of accumulating
/// entire segments in memory.
///
/// # Type Parameters
/// - `M`: Metadata type implementing StreamMetadata
/// - `D`: Data type implementing StreamData (usually Bytes)
///
/// # Usage
///
/// ```ignore
/// let mut decoder = GenericStreamDecoder::new();
///
/// for message in messages {
///     decoder.prepare_message(message).await?;
///     while let Some(chunk) = decoder.try_decode_chunk().await? {
///         play_audio(chunk);  // ~4KB per chunk
///     }
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
        debug!(?media_info, "creating decoder from shared chunked reader");

        let reader_clone = self.chunked_reader.clone();
        let decoder = SymphoniaDecoder::new_from_media_info(reader_clone, media_info, true)?;
        self.current_spec = Some(decoder.spec());
        self.decoder = Some(Box::new(decoder));

        debug!(spec = ?self.current_spec, "decoder created successfully");
        Ok(())
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

    async fn prepare_message(
        &mut self,
        message: StreamMessage<M, bytes::Bytes>,
    ) -> DecodeResult<()> {
        let (meta, data) = message.into_parts();

        self.messages_processed += 1;

        if self.messages_processed % 10 == 0 {
            debug!(
                messages_processed = self.messages_processed,
                boundaries = self.boundaries_encountered,
                data_size = data.len(),
                "GenericStreamDecoder stats"
            );
        }

        // Skip empty messages
        if data.is_empty() {
            if meta.is_boundary() {
                self.boundaries_encountered += 1;
            }
            return Ok(());
        }

        // Handle boundary (codec/format change)
        if meta.is_boundary() {
            self.boundaries_encountered += 1;

            debug!(
                sequence_id = meta.sequence_id(),
                boundaries_total = self.boundaries_encountered,
                "boundary detected - recreating decoder"
            );

            self.chunked_reader.clear();
            self.chunked_reader.append(data);

            let media_info = meta
                .media_info()
                .ok_or_else(|| DecodeError::DecodeError("No media info on boundary".to_string()))?;

            self.create_decoder(&media_info)?;
        } else {
            // No boundary - append data
            debug!(
                sequence_id = meta.sequence_id(),
                data_len = data.len(),
                "appending data to existing decoder"
            );

            self.chunked_reader.append(data);

            // Create decoder if first message
            if self.decoder.is_none() {
                let media_info = meta.media_info().ok_or_else(|| {
                    DecodeError::DecodeError("No media info for initial decoder".to_string())
                })?;
                self.create_decoder(&media_info)?;
            }
        }

        Ok(())
    }

    async fn try_decode_chunk(&mut self) -> DecodeResult<Option<Self::Output>> {
        let Some(decoder) = self.decoder.as_mut() else {
            return Ok(None);
        };

        match decoder.next_chunk() {
            Ok(Some(chunk)) => {
                self.current_spec = Some(chunk.spec);
                Ok(Some(chunk))
            }
            Ok(None) => {
                // No more data in current message
                self.chunked_reader.release_consumed();
                Ok(None)
            }
            Err(e) => {
                debug!(?e, "decode chunk error");
                self.chunked_reader.release_consumed();
                Ok(None)
            }
        }
    }

    async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
        debug!(
            messages_processed = self.messages_processed,
            boundaries = self.boundaries_encountered,
            "flushing decoder"
        );

        self.decoder = None;
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};

    use super::*;

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
        decoder.prepare_message(msg).await.unwrap();

        // Empty message - no chunks to decode
        let chunk = decoder.try_decode_chunk().await.unwrap();
        assert!(chunk.is_none());
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
        decoder.prepare_message(msg1).await.unwrap();

        assert_eq!(decoder.boundaries_encountered, 1);

        // Another boundary
        let meta2 = TestMetadata {
            seq: 2,
            boundary: true,
            codec: Some(AudioCodec::Mp3),
            container: None,
        };
        let msg2 = StreamMessage::new(meta2, Bytes::new());
        decoder.prepare_message(msg2).await.unwrap();

        assert_eq!(decoder.boundaries_encountered, 2);
    }

    #[tokio::test]
    async fn test_flush() {
        let mut decoder = GenericStreamDecoder::<TestMetadata, Bytes>::new();

        let remaining = decoder.flush().await.unwrap();
        assert_eq!(remaining.len(), 0);
        assert!(decoder.decoder.is_none());
    }
}
