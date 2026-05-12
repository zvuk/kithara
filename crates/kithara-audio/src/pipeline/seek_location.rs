use kithara_stream::{SourceSeekAnchor, StreamType};

use crate::pipeline::source::SharedStream;

/// Where the FSM expects its next useful byte to come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekLocation {
    /// Specific byte target (anchor, estimated direct-seek byte, or
    /// decoder-recreate offset).
    Byte {
        offset: u64,
        /// Variant the byte belongs to, when known. Currently
        /// informational — reserved for cross-variant readiness
        /// checks while layouts diverge.
        variant: Option<u32>,
    },
    /// No byte target known — fall back to the stream's current
    /// read head (`shared_stream.position()` / `shared_stream.phase()`).
    CurrentPosition,
}

impl SeekLocation {
    /// Canonical constructor from a resolved seek anchor.
    ///
    /// `variant_index` is Optional; on the (impossible in practice) overflow
    /// path we degrade to `None` rather than truncate, since `None` already
    /// means "variant is unknown" downstream.
    pub(crate) fn from_anchor(anchor: SourceSeekAnchor) -> Self {
        let variant = anchor.variant_index.and_then(|v| u32::try_from(v).ok());
        Self::Byte {
            variant,
            offset: anchor.byte_offset,
        }
    }

    /// Canonical constructor from an estimated direct-seek target byte.
    pub(crate) fn from_estimate(byte: u64) -> Self {
        Self::Byte {
            offset: byte,
            variant: None,
        }
    }

    /// Canonical constructor from a decoder-recreate offset.
    pub(crate) fn from_recreate_offset(offset: u64) -> Self {
        Self::Byte {
            offset,
            variant: None,
        }
    }

    /// Post a demand signal for this location's byte range.
    ///
    /// Posts a 1-byte demand at the target offset — enough to drive
    /// the downloader's segment-level scheduling. For `CurrentPosition`,
    /// uses the stream's current read head.
    pub(crate) fn submit_demand<T: StreamType>(&self, stream: &SharedStream<T>) {
        let start = match self {
            Self::Byte { offset, .. } => *offset,
            Self::CurrentPosition => stream.position(),
        };
        stream.demand_range(start..start.saturating_add(1));
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_stream::SourceSeekAnchor;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn from_anchor_copies_offset_and_variant() {
        let anchor = SourceSeekAnchor::new(1024, Duration::from_secs(5))
            .with_segment_end(Duration::from_secs(10))
            .with_segment_index(3)
            .with_variant_index(2);
        assert_eq!(
            SeekLocation::from_anchor(anchor),
            SeekLocation::Byte {
                offset: 1024,
                variant: Some(2),
            },
        );
    }

    #[kithara::test]
    fn from_anchor_without_variant_index() {
        let anchor = SourceSeekAnchor::new(2048, Duration::ZERO);
        assert_eq!(
            SeekLocation::from_anchor(anchor),
            SeekLocation::Byte {
                offset: 2048,
                variant: None,
            },
        );
    }

    #[kithara::test]
    fn from_estimate_stores_offset_without_variant() {
        assert_eq!(
            SeekLocation::from_estimate(8192),
            SeekLocation::Byte {
                offset: 8192,
                variant: None,
            },
        );
    }

    #[kithara::test]
    fn from_recreate_offset_stores_offset_without_variant() {
        assert_eq!(
            SeekLocation::from_recreate_offset(16_384),
            SeekLocation::Byte {
                offset: 16_384,
                variant: None,
            },
        );
    }

    #[kithara::test]
    fn seek_locations_preserve_equality() {
        assert_eq!(
            SeekLocation::from_estimate(100),
            SeekLocation::Byte {
                offset: 100,
                variant: None,
            },
        );
        assert_ne!(
            SeekLocation::from_estimate(100),
            SeekLocation::from_estimate(101),
        );
        assert_ne!(
            SeekLocation::from_estimate(100),
            SeekLocation::CurrentPosition,
        );
    }
}
