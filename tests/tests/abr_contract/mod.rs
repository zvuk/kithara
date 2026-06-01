//! ABR pull-driven contract tests (probe-based).
//!
//! Each test in this directory exercises a slice of the pull-driven
//! refactor spec at `.docs/plans/2026-05-11-abr-pull-driven-simplification.md`.
//! Tests are independent: there are no shared assertion bundles that
//! would let a regression in one invariant mask a regression in another.

pub(crate) mod helpers;

mod probe_baseline_no_switch;
mod probe_commit_sequence;
mod probe_demuxer_continuity;
mod probe_disk_inventory_parity;
mod probe_emit_count_contract;
mod probe_init_range;
mod probe_pcm_seam_continuity;
mod probe_seek_invalidates_stale_pending;
mod probe_track_continuity_invariants;
