use std::num::{NonZeroU32, NonZeroU64};

use crate::TrackFailureKind;

/// Exclusive decoded-source boundary represented by rendered PCM.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SourceEnd {
    /// Sample rate of the decoded source coordinate.
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    /// Opaque immutable mapping revision represented by this boundary.
    #[field(get, copy, with)]
    mapping_revision: Option<NonZeroU64>,
    /// Exclusive decoded source frame.
    #[field(get, copy)]
    frame: u64,
}

impl SourceEnd {
    /// Construct a decoded-source boundary.
    #[must_use]
    pub const fn new(frame: u64, sample_rate: NonZeroU32) -> Self {
        Self {
            sample_rate,
            frame,
            mapping_revision: None,
        }
    }
}

/// Fetch result from a worker source.
#[derive(Debug)]
pub enum Fetch<C> {
    /// Decoded data from the open source.
    Data {
        data: C,
        /// Exact decoded-source boundary represented by this rendered output.
        source_end: Option<SourceEnd>,
    },
    /// Natural end-of-stream from the open source.
    NaturalEof,
    /// Decoder or source failure from the open source.
    Failure { failure: TrackFailureKind },
}

impl<C> Fetch<C> {
    /// Create a data fetch.
    #[must_use]
    pub const fn data(data: C) -> Self {
        Self::Data {
            data,
            source_end: None,
        }
    }

    /// Create a natural end-of-stream marker.
    #[must_use]
    pub const fn eof() -> Self {
        Self::NaturalEof
    }

    /// Create a failure marker distinct from natural end-of-stream.
    #[must_use]
    pub const fn failure(failure: TrackFailureKind) -> Self {
        Self::Failure { failure }
    }

    /// Create rendered data with its exact decoded-source boundary.
    #[must_use]
    pub const fn rendered(data: C, source_end: SourceEnd) -> Self {
        Self::Data {
            data,
            source_end: Some(source_end),
        }
    }
}
#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn source_span_rejects_an_inverted_interval() {
        let rate = NonZeroU32::new(48_000).expect("test sample rate");

        assert_eq!(kithara_signal::SourceSpan::new(2, 1, rate, 1), None);
    }
}
