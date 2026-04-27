//! Fetch result with epoch tracking for seek invalidation.
//!
//! Carries a decoded PCM chunk (or any `C`) from the audio source worker
//! to the consumer with an epoch tag so stale chunks emitted before a
//! seek can be discarded. The `kind` distinguishes normal data from the
//! two terminal markers — natural EOF vs. transient failure — so the
//! consumer can finalise the track only on natural EOF.

/// Kind of fetch marker on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchKind {
    /// Normal PCM payload.
    Data,
    /// Natural end-of-stream — the track played out to its duration.
    NaturalEof,
    /// Decoder / source failure. Transient during seek recovery or a
    /// genuine unrecoverable error; never conflate with `NaturalEof`.
    Failure,
}

/// Fetch result from a worker source.
#[derive(Debug, Clone)]
pub struct Fetch<C> {
    /// The data chunk (ignored for non-`Data` kinds).
    pub data: C,
    /// Marker kind.
    pub kind: FetchKind,
    /// Epoch for seek invalidation (0 if unused).
    pub epoch: u64,
}

impl<C> Fetch<C> {
    /// Create a fetch result from the legacy `is_eof: bool` shape.
    ///
    /// `is_eof = true` maps to `FetchKind::NaturalEof`; `false` to
    /// `FetchKind::Data`. Prefer the explicit constructors below.
    pub fn new(data: C, is_eof: bool, epoch: u64) -> Self {
        let kind = if is_eof {
            FetchKind::NaturalEof
        } else {
            FetchKind::Data
        };
        Self { data, kind, epoch }
    }

    /// Explicit data chunk.
    pub fn data(data: C, epoch: u64) -> Self {
        Self {
            data,
            kind: FetchKind::Data,
            epoch,
        }
    }

    /// Explicit natural-EOF marker.
    pub fn natural_eof(data: C, epoch: u64) -> Self {
        Self {
            data,
            kind: FetchKind::NaturalEof,
            epoch,
        }
    }

    /// Explicit failure marker (distinct from natural EOF).
    pub fn failure(data: C, epoch: u64) -> Self {
        Self {
            data,
            kind: FetchKind::Failure,
            epoch,
        }
    }

    /// Consume and return the inner data.
    pub fn into_inner(self) -> C {
        self.data
    }

    /// True iff this is a natural-EOF marker. Does **not** cover
    /// `Failure` — callers must use `is_failure()` or `is_terminal()`
    /// for the broader "end of stream for any reason" check.
    pub fn is_eof(&self) -> bool {
        self.kind == FetchKind::NaturalEof
    }

    /// True iff this is a failure marker.
    pub fn is_failure(&self) -> bool {
        self.kind == FetchKind::Failure
    }

    /// True iff this is any terminal marker (natural EOF or failure).
    pub fn is_terminal(&self) -> bool {
        !matches!(self.kind, FetchKind::Data)
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
    use kithara_test_utils::kithara;

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
