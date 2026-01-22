//! Stream-based message types with metadata.
//!
//! This module provides generic types for stream-based architectures where
//! messages carry both metadata (codec, format, boundaries) and data (bytes, samples).
//!
//! # Design Philosophy
//!
//! Instead of random-access `Source` trait, stream-based processing uses sequential
//! messages with rich metadata. This enables:
//! - Explicit codec/format changes (variant switch in HLS)
//! - Segment boundaries (for decoder reinitialization)
//! - Generic testing (mockall-friendly)

use std::fmt;

/// Generic stream message with metadata and data.
///
/// # Type Parameters
/// - `M`: Metadata type (implements [`StreamMetadata`])
/// - `D`: Data type (implements [`StreamData`])
///
/// # Examples
///
/// ```ignore
/// use kithara_stream::{StreamMessage, StreamMetadata, StreamData};
///
/// #[derive(Clone)]
/// struct MyMetadata {
///     sequence: u64,
///     is_boundary: bool,
/// }
///
/// impl StreamMetadata for MyMetadata {
///     fn sequence_id(&self) -> u64 { self.sequence }
///     fn is_boundary(&self) -> bool { self.is_boundary }
/// }
///
/// type MyMessage = StreamMessage<MyMetadata, Vec<u8>>;
/// ```
#[derive(Clone)]
pub struct StreamMessage<M, D> {
    /// Message metadata (codec, format, boundaries, etc.)
    pub meta: M,

    /// Message data (bytes, PCM samples, etc.)
    pub data: D,
}

impl<M, D> StreamMessage<M, D> {
    /// Create a new stream message.
    pub fn new(meta: M, data: D) -> Self {
        Self { meta, data }
    }

    /// Get reference to metadata.
    pub fn metadata(&self) -> &M {
        &self.meta
    }

    /// Get reference to data.
    pub fn data(&self) -> &D {
        &self.data
    }

    /// Consume message and return parts.
    pub fn into_parts(self) -> (M, D) {
        (self.meta, self.data)
    }

    /// Map data to a different type.
    pub fn map_data<D2, F>(self, f: F) -> StreamMessage<M, D2>
    where
        F: FnOnce(D) -> D2,
    {
        StreamMessage {
            meta: self.meta,
            data: f(self.data),
        }
    }
}

impl<M, D> fmt::Debug for StreamMessage<M, D>
where
    M: fmt::Debug,
    D: StreamData,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamMessage")
            .field("meta", &self.meta)
            .field("data_len", &self.data.len())
            .finish()
    }
}

/// Metadata trait for stream messages.
///
/// Implementations provide information about message ordering and boundaries
/// (e.g., codec changes, variant switches in HLS).
///
/// # Required Methods
/// - [`sequence_id`](StreamMetadata::sequence_id): Unique identifier for ordering
/// - [`is_boundary`](StreamMetadata::is_boundary): Whether this message marks a processing boundary
///
/// # Boundary Semantics
///
/// Boundaries indicate points where downstream processors (decoders) need to
/// reinitialize or reset state:
/// - HLS variant switch (new codec/format)
/// - Seek operation (discontinuity)
/// - Format change (sample rate, channels)
pub trait StreamMetadata: Send + Sync + Clone + 'static {
    /// Unique sequence identifier for this message.
    ///
    /// Used for ordering, validation, and debugging.
    /// Must be monotonically increasing within a stream.
    fn sequence_id(&self) -> u64;

    /// Whether this message marks a processing boundary.
    ///
    /// Boundaries signal downstream processors to reinitialize:
    /// - Decoder reset on codec change
    /// - Buffer flush on discontinuity
    /// - State reset on format change
    ///
    /// # Examples
    /// - HLS init segment: `true`
    /// - HLS variant switch: `true`
    /// - Regular media segment: `false`
    fn is_boundary(&self) -> bool;
}

/// Data trait for stream messages.
///
/// Provides length information for buffering, backpressure, and logging.
///
/// # Type Parameters
/// - `Item`: Element type (e.g., `u8` for bytes, `f32` for PCM samples)
pub trait StreamData: Send + Sync + 'static {
    /// Element type (e.g., `u8`, `f32`)
    type Item;

    /// Number of items in this data.
    fn len(&self) -> usize;

    /// Whether data is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// Implementations for common types

impl StreamData for bytes::Bytes {
    type Item = u8;

    fn len(&self) -> usize {
        bytes::Bytes::len(self)
    }

    fn is_empty(&self) -> bool {
        bytes::Bytes::is_empty(self)
    }
}

impl StreamData for Vec<u8> {
    type Item = u8;

    fn len(&self) -> usize {
        Vec::len(self)
    }

    fn is_empty(&self) -> bool {
        Vec::is_empty(self)
    }
}

impl StreamData for Vec<f32> {
    type Item = f32;

    fn len(&self) -> usize {
        Vec::len(self)
    }

    fn is_empty(&self) -> bool {
        Vec::is_empty(self)
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn test_stream_message_creation() {
        let meta = TestMetadata {
            seq: 42,
            boundary: false,
        };
        let data = vec![1u8, 2, 3];

        let msg = StreamMessage::new(meta.clone(), data.clone());

        assert_eq!(msg.metadata().seq, 42);
        assert_eq!(msg.data(), &data);
    }

    #[test]
    fn test_stream_message_into_parts() {
        let meta = TestMetadata {
            seq: 1,
            boundary: true,
        };
        let data = vec![1u8, 2, 3];

        let msg = StreamMessage::new(meta.clone(), data.clone());
        let (m, d) = msg.into_parts();

        assert_eq!(m.seq, 1);
        assert!(m.boundary);
        assert_eq!(d, data);
    }

    #[test]
    fn test_stream_message_map_data() {
        let meta = TestMetadata {
            seq: 1,
            boundary: false,
        };
        let data = vec![1u8, 2, 3];

        let msg = StreamMessage::new(meta, data);
        let mapped = msg.map_data(|d| d.len());

        assert_eq!(mapped.data, 3);
    }

    #[test]
    fn test_stream_data_bytes() {
        let data = bytes::Bytes::from(vec![1u8, 2, 3]);
        assert_eq!(data.len(), 3);
        assert!(!data.is_empty());

        let empty = bytes::Bytes::new();
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }

    #[test]
    fn test_stream_data_vec_u8() {
        let data = vec![1u8, 2, 3];
        assert_eq!(data.len(), 3);
        assert!(!data.is_empty());
    }

    #[test]
    fn test_stream_data_vec_f32() {
        let data = vec![1.0f32, 2.0, 3.0];
        assert_eq!(data.len(), 3);
        assert!(!data.is_empty());
    }

    #[test]
    fn test_metadata_boundary_semantics() {
        let regular = TestMetadata {
            seq: 1,
            boundary: false,
        };
        assert!(!regular.is_boundary());

        let boundary = TestMetadata {
            seq: 2,
            boundary: true,
        };
        assert!(boundary.is_boundary());
    }
}
