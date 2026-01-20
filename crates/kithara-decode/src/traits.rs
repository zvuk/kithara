//! Traits for testability and abstraction.

#[cfg(any(test, feature = "test-utils"))]
use mockall::automock;

use crate::PcmSpec;

/// Trait for PCM buffer abstraction.
///
/// Allows PcmSource to be generic over buffer implementation for testing.
#[cfg_attr(any(test, feature = "test-utils"), automock)]
pub trait PcmBufferTrait: Send + Sync {
    /// Get PCM specification.
    fn spec(&self) -> PcmSpec;

    /// Get total frames written.
    fn frames_written(&self) -> u64;

    /// Check if EOF has been reached.
    fn is_eof(&self) -> bool;

    /// Read samples at given frame offset.
    ///
    /// Returns number of samples read (not frames).
    fn read_samples(&self, frame_offset: u64, buf: &mut [f32]) -> usize;
}
