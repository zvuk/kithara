//! Probes: `Source::position`, `Source::advance`.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-6`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Plan 02 fills
//! the Source-position/advance scenarios.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn stream_read_advances_position_on_success() {
    unimplemented!("Plan 02 — stream_read_advances_position_on_success scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn stream_read_does_not_advance_on_eof() {
    unimplemented!("Plan 02 — stream_read_does_not_advance_on_eof scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn stream_read_outcome_byte_position_uses_source_position() {
    unimplemented!("Plan 02 — stream_read_outcome_byte_position_uses_source_position scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn position_read_concurrent_no_torn_reads() {
    unimplemented!("Plan 02 — position_read_concurrent_no_torn_reads scenario");
}
