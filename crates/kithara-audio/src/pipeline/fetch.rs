/// Fetch result from a worker source.
#[derive(Debug)]
pub enum Fetch<C> {
    /// Decoded data for an epoch.
    Data { data: C, epoch: u64 },
    /// Natural end-of-stream for an epoch.
    NaturalEof { epoch: u64 },
    /// Decoder or source failure for an epoch.
    Failure { epoch: u64 },
}

impl<C> Fetch<C> {
    /// Create a data fetch.
    #[must_use]
    pub fn data(data: C, epoch: u64) -> Self {
        Self::Data { data, epoch }
    }

    /// Create a natural end-of-stream marker.
    #[must_use]
    pub fn eof(epoch: u64) -> Self {
        Self::NaturalEof { epoch }
    }

    /// Create a failure marker distinct from natural end-of-stream.
    #[must_use]
    pub fn failure(epoch: u64) -> Self {
        Self::Failure { epoch }
    }

    /// Return the seek-invalidation epoch.
    pub const fn epoch(&self) -> u64 {
        match self {
            Self::Data { epoch, .. } | Self::NaturalEof { epoch } | Self::Failure { epoch } => {
                *epoch
            }
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
    pub fn is_valid<C>(&self, item: &Fetch<C>) -> bool {
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
}
