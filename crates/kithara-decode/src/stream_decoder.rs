//! Stream-based decoder trait for processing messages with metadata.
//!
//! This module provides a generic decoder interface that processes sequential
//! stream messages (metadata + data) using incremental decoding.
//!
//! # Architecture
//!
//! Stream-based decoders process messages incrementally:
//! ```ignore
//! let decoder = StreamDecoder::new();
//! for message in stream {
//!     decoder.prepare_message(message).await?;
//!     while let Some(chunk) = decoder.try_decode_chunk().await? {
//!         play_audio(chunk);  // ~4KB per chunk
//!     }
//! }
//! ```
//!
//! # Benefits
//!
//! - **Memory efficient**: Each chunk is small (~4KB), no accumulation
//! - **Explicit boundaries**: Decoder knows when to reinitialize
//! - **Testable**: Generic + mockall support for unit tests

use async_trait::async_trait;
use kithara_stream::{StreamData, StreamMessage, StreamMetadata};

use crate::types::DecodeResult;

/// Generic stream decoder that processes messages incrementally.
///
/// # Type Parameters
/// - `M`: Metadata type (implements [`StreamMetadata`])
/// - `D`: Data type (implements [`StreamData`])
///
/// # Usage
///
/// ```ignore
/// // Prepare message (handles boundary detection, decoder init)
/// decoder.prepare_message(message).await?;
///
/// // Decode incrementally (~4KB chunks)
/// while let Some(chunk) = decoder.try_decode_chunk().await? {
///     play_audio(chunk);
/// }
/// ```
#[async_trait]
pub trait StreamDecoder<M, D>: Send + Sync
where
    M: StreamMetadata,
    D: StreamData,
{
    /// Output type (e.g., PCM chunks, frames, packets)
    type Output: Send;

    /// Prepare a stream message for incremental decoding.
    ///
    /// This method:
    /// 1. Handles boundary detection and decoder reinitialization
    /// 2. Appends message data to internal buffer
    ///
    /// After calling this, use `try_decode_chunk()` in a loop.
    async fn prepare_message(&mut self, message: StreamMessage<M, D>) -> DecodeResult<()>;

    /// Try to decode the next chunk from prepared data.
    ///
    /// Returns:
    /// - `Ok(Some(chunk))` - A decoded chunk (~1024 samples for AAC)
    /// - `Ok(None)` - No more data to decode from current message
    /// - `Err(e)` - Decode error
    async fn try_decode_chunk(&mut self) -> DecodeResult<Option<Self::Output>>;

    /// Flush any pending output data.
    ///
    /// Called at end of stream to ensure all queued data is processed.
    async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>>;
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use kithara_stream::StreamMessage;

    use super::*;

    #[derive(Debug, Clone)]
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

    struct TestDecoder {
        outputs: Vec<String>,
        reinit_count: usize,
        pending: Option<String>,
    }

    impl TestDecoder {
        fn new() -> Self {
            Self {
                outputs: vec![],
                reinit_count: 0,
                pending: None,
            }
        }
    }

    #[async_trait]
    impl StreamDecoder<TestMetadata, Bytes> for TestDecoder {
        type Output = String;

        async fn prepare_message(
            &mut self,
            message: StreamMessage<TestMetadata, Bytes>,
        ) -> DecodeResult<()> {
            if message.meta.is_boundary() {
                self.reinit_count += 1;
            }
            self.pending = Some(format!(
                "decoded_seq_{}_len_{}",
                message.meta.sequence_id(),
                message.data.len()
            ));
            Ok(())
        }

        async fn try_decode_chunk(&mut self) -> DecodeResult<Option<Self::Output>> {
            if let Some(output) = self.pending.take() {
                self.outputs.push(output.clone());
                Ok(Some(output))
            } else {
                Ok(None)
            }
        }

        async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
            Ok(vec!["flushed".to_string()])
        }
    }

    #[tokio::test]
    async fn test_incremental_decode() {
        let mut decoder = TestDecoder::new();

        let msg = StreamMessage::new(
            TestMetadata {
                seq: 1,
                boundary: false,
            },
            Bytes::from(vec![1, 2, 3]),
        );

        decoder.prepare_message(msg).await.unwrap();

        let chunk = decoder.try_decode_chunk().await.unwrap();
        assert_eq!(chunk, Some("decoded_seq_1_len_3".to_string()));

        let chunk = decoder.try_decode_chunk().await.unwrap();
        assert_eq!(chunk, None);
    }

    #[tokio::test]
    async fn test_boundary_handling() {
        let mut decoder = TestDecoder::new();

        // Regular message
        let msg1 = StreamMessage::new(
            TestMetadata {
                seq: 1,
                boundary: false,
            },
            Bytes::from(vec![1, 2, 3]),
        );
        decoder.prepare_message(msg1).await.unwrap();
        while decoder.try_decode_chunk().await.unwrap().is_some() {}

        // Boundary message
        let msg2 = StreamMessage::new(
            TestMetadata {
                seq: 2,
                boundary: true,
            },
            Bytes::from(vec![4, 5, 6]),
        );
        decoder.prepare_message(msg2).await.unwrap();
        while decoder.try_decode_chunk().await.unwrap().is_some() {}

        assert_eq!(decoder.outputs.len(), 2);
        assert_eq!(decoder.reinit_count, 1);
    }

    #[tokio::test]
    async fn test_flush() {
        let mut decoder = TestDecoder::new();
        let flushed = decoder.flush().await.unwrap();
        assert_eq!(flushed, vec!["flushed".to_string()]);
    }

    #[tokio::test]
    async fn test_multiple_messages() {
        let mut decoder = TestDecoder::new();

        for i in 0..5 {
            let msg = StreamMessage::new(
                TestMetadata {
                    seq: i,
                    boundary: i % 2 == 0,
                },
                Bytes::from(vec![i as u8]),
            );
            decoder.prepare_message(msg).await.unwrap();
            while decoder.try_decode_chunk().await.unwrap().is_some() {}
        }

        assert_eq!(decoder.outputs.len(), 5);
        assert_eq!(decoder.reinit_count, 3); // seq 0, 2, 4
    }
}
