//! Disk inventory equals dispatched emits — set equality on (variant, segment).
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-12`
//!
//! Plan 00 skeleton — body panics via `unimplemented!()`. Replaces T3.
//! Plan 10 fills the disk-inventory-parity scenario.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn disk_files_match_dispatched_emits() {
    unimplemented!("Plan 10 — disk_files_match_dispatched_emits scenario");
}
