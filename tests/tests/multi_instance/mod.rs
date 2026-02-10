//! Multi-instance integration tests.
//!
//! Verifies that the library works correctly with multiple concurrent
//! `Audio` instances sharing a rayon `ThreadPool`. Tests 2, 4, and 8
//! instances in various combinations (File, HLS, mixed) and validates
//! that network failures in some instances do not affect others.

mod concurrent_file;
mod concurrent_hls;
mod concurrent_mixed;
mod failure_resilience;
