//! Item validation for epoch-based invalidation.

use crate::item::{Fetch, WorkerItem};

/// Trait for validating worker items.
///
/// Different validators can be used depending on whether epoch tracking is needed.
pub trait ItemValidator<I: WorkerItem> {
    /// Check if an item is valid.
    fn is_valid(&self, item: &I) -> bool;
}

/// Validator that checks epoch for invalidation.
///
/// Used with `EpochItem` to discard outdated items after seek.
#[derive(Debug, Clone)]
pub struct EpochValidator {
    /// Current consumer epoch.
    pub epoch: u64,
}

impl EpochValidator {
    /// Create a new epoch validator.
    pub fn new() -> Self {
        Self { epoch: 0 }
    }

    /// Increment epoch (called on seek).
    pub fn next_epoch(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }
}

impl<C: Send + 'static> ItemValidator<Fetch<C>> for EpochValidator {
    fn is_valid(&self, item: &Fetch<C>) -> bool {
        item.epoch == self.epoch
    }
}

impl Default for EpochValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// Validator that accepts all items (no invalidation).
///
/// Used with `SimpleItem` when seeking is not supported.
#[derive(Debug, Clone, Copy)]
pub struct AlwaysValid;

impl<C: Send + 'static> ItemValidator<Fetch<C>> for AlwaysValid {
    fn is_valid(&self, _item: &Fetch<C>) -> bool {
        true
    }
}

/// Consumer-side helper for processing items with epoch validation.
///
/// This is an alias for `EpochValidator` for backward compatibility.
pub type EpochConsumer = EpochValidator;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epoch_validator() {
        let mut validator = EpochValidator::new();
        assert_eq!(validator.epoch, 0);

        let item0 = Fetch::new(42, false, 0);
        assert!(validator.is_valid(&item0));

        validator.next_epoch();
        assert!(!validator.is_valid(&item0));

        let item1 = Fetch::new(43, false, 1);
        assert!(validator.is_valid(&item1));
    }

    #[test]
    fn test_always_valid_validator() {
        let validator = AlwaysValid;
        let item = Fetch::new(42, false, 0);
        assert!(validator.is_valid(&item));

        let eof_item = Fetch::new(0, true, 0);
        assert!(validator.is_valid(&eof_item));
    }
}
