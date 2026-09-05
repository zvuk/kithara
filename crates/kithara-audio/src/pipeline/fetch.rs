use std::num::NonZeroU32;

/// Exclusive decoded-source boundary represented by rendered PCM.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SourceEnd {
    /// Sample rate of the decoded source coordinate.
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    /// Exclusive decoded source frame.
    #[field(get, copy)]
    frame: u64,
}

impl SourceEnd {
    /// Construct a decoded-source boundary.
    #[must_use]
    pub const fn new(frame: u64, sample_rate: NonZeroU32) -> Self {
        Self { sample_rate, frame }
    }
}

/// Exact decoded-source interval represented by rendered PCM.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SourceSpan {
    /// Inclusive decoded source frame.
    #[field(get, copy)]
    start: u64,
    /// Exclusive decoded source frame.
    #[field(get, copy)]
    end: u64,
    /// Sample rate of the decoded source coordinate.
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    /// Opaque producer render revision represented by this output span.
    #[field(get, copy, with)]
    render_revision: u64,
}

impl SourceSpan {
    /// Construct a decoded-source interval.
    #[must_use]
    pub const fn new(start: u64, end: u64, sample_rate: NonZeroU32) -> Option<Self> {
        if start > end {
            return None;
        }
        Some(Self {
            start,
            end,
            sample_rate,
            render_revision: 0,
        })
    }
}

/// Fetch result from a worker source.
#[derive(Debug)]
pub enum Fetch<C> {
    /// Decoded data for an epoch.
    Data {
        data: C,
        epoch: u64,
        /// Exact decoded-source boundary represented by this rendered output.
        source_end: Option<SourceEnd>,
    },
    /// Natural end-of-stream for an epoch.
    NaturalEof { epoch: u64 },
    /// Decoder or source failure for an epoch.
    Failure { epoch: u64 },
}

impl<C> Fetch<C> {
    /// Create a data fetch.
    #[must_use]
    pub const fn data(data: C, epoch: u64) -> Self {
        Self::Data {
            data,
            epoch,
            source_end: None,
        }
    }

    /// Create a natural end-of-stream marker.
    #[must_use]
    pub const fn eof(epoch: u64) -> Self {
        Self::NaturalEof { epoch }
    }

    /// Return the seek-invalidation epoch.
    pub const fn epoch(&self) -> u64 {
        match self {
            Self::Data { epoch, .. } | Self::NaturalEof { epoch } | Self::Failure { epoch } => {
                *epoch
            }
        }
    }

    /// Create a failure marker distinct from natural end-of-stream.
    #[must_use]
    pub const fn failure(epoch: u64) -> Self {
        Self::Failure { epoch }
    }

    /// Create rendered data with its exact decoded-source boundary.
    #[must_use]
    pub const fn rendered(data: C, epoch: u64, source_end: SourceEnd) -> Self {
        Self::Data {
            data,
            epoch,
            source_end: Some(source_end),
        }
    }
}

/// Validator that checks epoch for seek invalidation.
///
/// Consumer increments epoch on seek; items with old epoch are discarded.
#[derive(Debug, Clone, Default)]
pub struct EpochValidator {
    /// Current consumer epoch.
    pub epoch: u64,
}

impl EpochValidator {
    /// Check if a fetch result matches the current epoch.
    pub const fn is_valid<C>(&self, item: &Fetch<C>) -> bool {
        item.epoch() == self.epoch
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn epoch_validator_keeps_matching_chunks() {
        let mut validator = EpochValidator::default();
        let item = Fetch::data(vec![1u8, 2, 3], 1);
        validator.epoch = 1;
        assert!(validator.is_valid(&item));
    }

    #[kithara::test]
    fn epoch_validator_rejects_stale_chunks_after_seek() {
        let mut validator = EpochValidator::default();
        let stale = Fetch::data(vec![3u8], validator.epoch);
        let first = Fetch::data(vec![1u8], validator.epoch);
        validator.epoch = validator.epoch.wrapping_add(1);
        let next = Fetch::data(vec![2u8], validator.epoch);

        assert!(!validator.is_valid(&first));
        assert!(!validator.is_valid(&stale));
        assert!(validator.is_valid(&next));
    }

    #[kithara::test]
    fn source_span_rejects_an_inverted_interval() {
        let rate = NonZeroU32::new(48_000).expect("test sample rate");

        assert_eq!(SourceSpan::new(2, 1, rate), None);
    }
}
