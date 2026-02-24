//! Fetch result with epoch tracking for seek invalidation.
//!
//! Used by both async (Backend) and sync (decode) workers
//! to deliver data with epoch-based stale chunk detection.

/// Fetch result from a worker source.
#[derive(Debug, Clone)]
pub struct Fetch<C> {
    /// The data chunk.
    pub data: C,
    /// Whether this is the final chunk (EOF).
    pub is_eof: bool,
    /// Epoch for seek invalidation (0 if unused).
    pub epoch: u64,
}

impl<C> Fetch<C> {
    /// Create a new fetch result.
    pub fn new(data: C, is_eof: bool, epoch: u64) -> Self {
        Self {
            data,
            is_eof,
            epoch,
        }
    }

    /// Consume and return the inner data.
    pub fn into_inner(self) -> C {
        self.data
    }

    /// Check if this is an EOF marker.
    pub fn is_eof(&self) -> bool {
        self.is_eof
    }

    /// Get the epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// Validator that checks epoch for seek invalidation.
///
/// Consumer increments epoch on seek; items with old epoch are discarded.
#[derive(Debug, Clone)]
pub struct EpochValidator {
    /// Current consumer epoch.
    pub epoch: u64,
}

impl EpochValidator {
    /// Create a new epoch validator.
    #[must_use]
    pub fn new() -> Self {
        Self { epoch: 0 }
    }

    /// Increment epoch (called on seek). Returns new epoch.
    pub fn next_epoch(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }

    /// Check if a fetch result matches the current epoch.
    pub fn is_valid<C>(&self, item: &Fetch<C>) -> bool {
        item.epoch == self.epoch
    }
}

impl Default for EpochValidator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[kithara::test]
    fn epoch_validator_keeps_matching_chunks() {
        let mut validator = EpochValidator::new();
        let item = Fetch::new(vec![1u8, 2, 3], false, 1);
        validator.epoch = 1;
        assert!(validator.is_valid(&item));
    }

    #[kithara::test]
    fn epoch_validator_rejects_stale_chunks_after_seek() {
        let mut validator = EpochValidator::new();
        let stale = Fetch::new(vec![3u8], false, validator.epoch);
        let first = Fetch::new(vec![1u8], false, validator.epoch);
        let next_epoch = validator.next_epoch();
        let next = Fetch::new(vec![2u8], false, next_epoch);

        assert!(!validator.is_valid(&first));
        assert!(!validator.is_valid(&stale));
        assert!(validator.is_valid(&next));
    }
}
