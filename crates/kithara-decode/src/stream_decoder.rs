//! Stream-based decoder trait for processing messages with metadata.
//!
//! This module provides a generic decoder interface that processes sequential
//! stream messages (metadata + data) instead of random-access byte streams.
//!
//! # Architecture
//!
//! Traditional decoders expect `Read + Seek`:
//! ```ignore
//! let decoder = Decoder::new(reader)?;
//! loop {
//!     let pcm = decoder.decode_next()?;
//! }
//! ```
//!
//! Stream-based decoders process messages with metadata:
//! ```ignore
//! let decoder = StreamDecoder::new();
//! for message in stream {
//!     if message.meta.is_boundary() {
//!         decoder.reinitialize(&message.meta)?;
//!     }
//!     let pcm = decoder.decode_message(message)?;
//! }
//! ```
//!
//! # Benefits
//!
//! - **Explicit boundaries**: Decoder knows when to reinitialize (codec change, variant switch)
//! - **No random access**: Simpler data flow, less buffering
//! - **Testable**: Generic + mockall support for unit tests
//! - **HLS ABR-friendly**: Handles variant switches cleanly

use async_trait::async_trait;
use kithara_stream::{StreamData, StreamMessage, StreamMetadata};

use crate::types::DecodeResult;

/// Generic stream decoder that processes messages with metadata.
///
/// # Type Parameters
/// - `M`: Metadata type (implements [`StreamMetadata`])
/// - `D`: Data type (implements [`StreamData`])
///
/// # Boundary Handling
///
/// Decoders check `meta.is_boundary()` to decide when reinitialization is needed:
/// - HLS variant switch → reinitialize with new codec
/// - Seek operation → flush internal buffers
/// - Format change → reconfigure decoder
///
/// # Examples
///
/// ```ignore
/// use kithara_decode::{StreamDecoder, PcmChunk};
/// use kithara_stream::StreamMessage;
///
/// async fn decode_stream<D>(mut decoder: D, messages: Vec<StreamMessage<M, D>>)
/// where
///     D: StreamDecoder<M, D>,
/// {
///     for msg in messages {
///         if msg.meta.is_boundary() {
///             // Decoder handles reinitialization internally
///         }
///         let pcm = decoder.decode_message(msg).await?;
///         play_audio(pcm);
///     }
/// }
/// ```
///
/// # Testing with Mockall
///
/// ```ignore
/// use mockall::mock;
///
/// mock! {
///     pub MyDecoder {}
///
///     #[async_trait]
///     impl<M, D> StreamDecoder<M, D> for MyDecoder
///     where
///         M: StreamMetadata,
///         D: StreamData,
///     {
///         type Output = MyOutput;
///
///         async fn decode_message(&mut self, msg: StreamMessage<M, D>)
///             -> DecodeResult<Self::Output>;
///         async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>>;
///     }
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

    /// Decode a stream message.
    ///
    /// The decoder checks `message.meta.is_boundary()` to determine if
    /// reinitialization is needed (e.g., codec change on HLS variant switch).
    ///
    /// # Boundary Handling
    ///
    /// When `meta.is_boundary()` is true, the decoder should:
    /// 1. Flush any pending data from previous codec/format
    /// 2. Reinitialize with new metadata (codec, sample rate, etc.)
    /// 3. Process the new message data
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if:
    /// - Codec initialization fails
    /// - Data is corrupted
    /// - Unsupported format
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let message = StreamMessage {
    ///     meta: HlsSegmentMetadata {
    ///         is_variant_switch: true,
    ///         codec: Some(AudioCodec::AAC),
    ///         // ...
    ///     },
    ///     data: bytes,
    /// };
    ///
    /// let pcm_chunk = decoder.decode_message(message).await?;
    /// ```
    async fn decode_message(
        &mut self,
        message: StreamMessage<M, D>,
    ) -> DecodeResult<Self::Output>;

    /// Flush any pending output data.
    ///
    /// Called at end of stream or before seek to ensure all queued data
    /// is processed.
    ///
    /// # Returns
    ///
    /// Vector of output chunks (may be empty if nothing buffered).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // End of stream
    /// let remaining = decoder.flush().await?;
    /// for chunk in remaining {
    ///     play_audio(chunk);
    /// }
    /// ```
    async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use kithara_stream::StreamMessage;

    use crate::types::DecodeError;

    // Test metadata type
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

    // Test decoder implementation
    struct TestDecoder {
        outputs: Vec<String>,
        reinit_count: usize,
    }

    #[async_trait]
    impl StreamDecoder<TestMetadata, Bytes> for TestDecoder {
        type Output = String;

        async fn decode_message(
            &mut self,
            message: StreamMessage<TestMetadata, Bytes>,
        ) -> DecodeResult<Self::Output> {
            if message.meta.is_boundary() {
                self.reinit_count += 1;
            }

            let output = format!(
                "decoded_seq_{}_len_{}",
                message.meta.sequence_id(),
                message.data.len()
            );
            self.outputs.push(output.clone());
            Ok(output)
        }

        async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
            Ok(vec!["flushed".to_string()])
        }
    }

    #[tokio::test]
    async fn test_decoder_processes_messages() {
        let mut decoder = TestDecoder {
            outputs: vec![],
            reinit_count: 0,
        };

        let msg = StreamMessage::new(
            TestMetadata {
                seq: 1,
                boundary: false,
            },
            Bytes::from(vec![1, 2, 3]),
        );

        let output = decoder.decode_message(msg).await.unwrap();
        assert_eq!(output, "decoded_seq_1_len_3");
        assert_eq!(decoder.outputs.len(), 1);
        assert_eq!(decoder.reinit_count, 0);
    }

    #[tokio::test]
    async fn test_decoder_handles_boundaries() {
        let mut decoder = TestDecoder {
            outputs: vec![],
            reinit_count: 0,
        };

        // Regular message
        let msg1 = StreamMessage::new(
            TestMetadata {
                seq: 1,
                boundary: false,
            },
            Bytes::from(vec![1, 2, 3]),
        );
        decoder.decode_message(msg1).await.unwrap();

        // Boundary message (e.g., variant switch)
        let msg2 = StreamMessage::new(
            TestMetadata {
                seq: 2,
                boundary: true,
            },
            Bytes::from(vec![4, 5, 6]),
        );
        decoder.decode_message(msg2).await.unwrap();

        assert_eq!(decoder.outputs.len(), 2);
        assert_eq!(decoder.reinit_count, 1); // Reinitialized on boundary
    }

    #[tokio::test]
    async fn test_decoder_flush() {
        let mut decoder = TestDecoder {
            outputs: vec![],
            reinit_count: 0,
        };

        let flushed = decoder.flush().await.unwrap();
        assert_eq!(flushed, vec!["flushed".to_string()]);
    }

    #[tokio::test]
    async fn test_multiple_boundaries() {
        let mut decoder = TestDecoder {
            outputs: vec![],
            reinit_count: 0,
        };

        // Simulate multiple variant switches
        for i in 0..5 {
            let msg = StreamMessage::new(
                TestMetadata {
                    seq: i,
                    boundary: i % 2 == 0, // Every other message is boundary
                },
                Bytes::from(vec![i as u8]),
            );
            decoder.decode_message(msg).await.unwrap();
        }

        assert_eq!(decoder.outputs.len(), 5);
        assert_eq!(decoder.reinit_count, 3); // seq 0, 2, 4
    }

    // Mock decoder that tracks method calls
    struct MockTrackingDecoder {
        decode_calls: Vec<u64>,
        flush_calls: usize,
        should_fail_at: Option<u64>,
    }

    #[async_trait]
    impl StreamDecoder<TestMetadata, Bytes> for MockTrackingDecoder {
        type Output = String;

        async fn decode_message(
            &mut self,
            message: StreamMessage<TestMetadata, Bytes>,
        ) -> DecodeResult<Self::Output> {
            let seq = message.meta.sequence_id();
            self.decode_calls.push(seq);

            // Simulate failure at specific sequence
            if self.should_fail_at == Some(seq) {
                return Err(DecodeError::DecodeError(format!("Mock error at seq {}", seq)));
            }

            Ok(format!("mock_output_{}", seq))
        }

        async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
            self.flush_calls += 1;
            Ok(vec![format!("flush_{}", self.flush_calls)])
        }
    }

    #[tokio::test]
    async fn test_mock_decoder_tracking() {
        let mut decoder = MockTrackingDecoder {
            decode_calls: vec![],
            flush_calls: 0,
            should_fail_at: None,
        };

        // Process several messages
        for i in 0..3 {
            let msg = StreamMessage::new(
                TestMetadata {
                    seq: i,
                    boundary: false,
                },
                Bytes::new(),
            );
            decoder.decode_message(msg).await.unwrap();
        }

        // Verify all calls were tracked
        assert_eq!(decoder.decode_calls, vec![0, 1, 2]);
        assert_eq!(decoder.flush_calls, 0);

        // Flush
        let result = decoder.flush().await.unwrap();
        assert_eq!(result, vec!["flush_1"]);
        assert_eq!(decoder.flush_calls, 1);
    }

    #[tokio::test]
    async fn test_mock_decoder_error_handling() {
        let mut decoder = MockTrackingDecoder {
            decode_calls: vec![],
            flush_calls: 0,
            should_fail_at: Some(2),
        };

        // First message succeeds
        let msg1 = StreamMessage::new(
            TestMetadata {
                seq: 1,
                boundary: false,
            },
            Bytes::new(),
        );
        assert!(decoder.decode_message(msg1).await.is_ok());

        // Second message fails
        let msg2 = StreamMessage::new(
            TestMetadata {
                seq: 2,
                boundary: false,
            },
            Bytes::new(),
        );
        let result = decoder.decode_message(msg2).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "decode error: Mock error at seq 2");

        // Verify both calls were attempted
        assert_eq!(decoder.decode_calls, vec![1, 2]);
    }

    // Mock decoder with stateful behavior
    struct StatefulMockDecoder {
        state: String,
        message_count: usize,
    }

    #[async_trait]
    impl StreamDecoder<TestMetadata, Bytes> for StatefulMockDecoder {
        type Output = String;

        async fn decode_message(
            &mut self,
            message: StreamMessage<TestMetadata, Bytes>,
        ) -> DecodeResult<Self::Output> {
            self.message_count += 1;

            // Update state on boundary
            if message.meta.is_boundary() {
                self.state = format!("boundary_at_{}", message.meta.sequence_id());
            }

            Ok(format!("{}:msg_{}", self.state, self.message_count))
        }

        async fn flush(&mut self) -> DecodeResult<Vec<Self::Output>> {
            Ok(vec![format!("final_state:{}", self.state)])
        }
    }

    #[tokio::test]
    async fn test_stateful_mock_decoder() {
        let mut decoder = StatefulMockDecoder {
            state: "initial".to_string(),
            message_count: 0,
        };

        // Regular message
        let msg1 = StreamMessage::new(
            TestMetadata {
                seq: 1,
                boundary: false,
            },
            Bytes::new(),
        );
        let result1 = decoder.decode_message(msg1).await.unwrap();
        assert_eq!(result1, "initial:msg_1");

        // Boundary message changes state
        let msg2 = StreamMessage::new(
            TestMetadata {
                seq: 2,
                boundary: true,
            },
            Bytes::new(),
        );
        let result2 = decoder.decode_message(msg2).await.unwrap();
        assert_eq!(result2, "boundary_at_2:msg_2");

        // Next message uses updated state
        let msg3 = StreamMessage::new(
            TestMetadata {
                seq: 3,
                boundary: false,
            },
            Bytes::new(),
        );
        let result3 = decoder.decode_message(msg3).await.unwrap();
        assert_eq!(result3, "boundary_at_2:msg_3");

        // Flush returns final state
        let flushed = decoder.flush().await.unwrap();
        assert_eq!(flushed, vec!["final_state:boundary_at_2"]);
    }

    #[tokio::test]
    async fn test_empty_data_handling() {
        let mut decoder = TestDecoder {
            outputs: vec![],
            reinit_count: 0,
        };

        // Empty bytes
        let msg = StreamMessage::new(
            TestMetadata {
                seq: 1,
                boundary: false,
            },
            Bytes::new(),
        );

        let output = decoder.decode_message(msg).await.unwrap();
        assert_eq!(output, "decoded_seq_1_len_0");
    }

    #[tokio::test]
    async fn test_large_sequence() {
        let mut decoder = TestDecoder {
            outputs: vec![],
            reinit_count: 0,
        };

        // Process 100 messages
        for i in 0..100 {
            let msg = StreamMessage::new(
                TestMetadata {
                    seq: i,
                    boundary: i % 10 == 0, // Boundary every 10 messages
                },
                Bytes::from(vec![i as u8]),
            );
            decoder.decode_message(msg).await.unwrap();
        }

        assert_eq!(decoder.outputs.len(), 100);
        assert_eq!(decoder.reinit_count, 10); // 10 boundaries
    }

    #[tokio::test]
    async fn test_consecutive_boundaries() {
        let mut decoder = TestDecoder {
            outputs: vec![],
            reinit_count: 0,
        };

        // Multiple consecutive boundaries
        for i in 0..5 {
            let msg = StreamMessage::new(
                TestMetadata {
                    seq: i,
                    boundary: true, // All boundaries
                },
                Bytes::new(),
            );
            decoder.decode_message(msg).await.unwrap();
        }

        assert_eq!(decoder.outputs.len(), 5);
        assert_eq!(decoder.reinit_count, 5); // Every message triggered reinit
    }

    #[tokio::test]
    async fn test_multiple_flush_calls() {
        let mut decoder = TestDecoder {
            outputs: vec![],
            reinit_count: 0,
        };

        // First flush
        let result1 = decoder.flush().await.unwrap();
        assert_eq!(result1, vec!["flushed"]);

        // Second flush (should still work)
        let result2 = decoder.flush().await.unwrap();
        assert_eq!(result2, vec!["flushed"]);

        // Verify decoder state is consistent
        assert_eq!(decoder.outputs.len(), 0);
        assert_eq!(decoder.reinit_count, 0);
    }
}
