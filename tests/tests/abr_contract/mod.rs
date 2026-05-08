//! ABR-switch contract tests T1..T8.
//!
//! Each test in this directory enforces a subset of the axioms
//! catalogued in `.docs/plans/2026-05-03-abr-switch-contract.md`.
//! Tests are independent: there are no shared assertion bundles that
//! would let a regression in one axiom mask a regression in another.

pub(super) mod helpers;

mod t1_phase_continuity_wave;
mod t2_exact_fetch_count;
mod t3_disk_equals_network;
mod t4_seek_freezes_abr;
mod t5_switch_latency_bounded;
mod t6_init_range_outcome_ok_seg0;
mod t7_no_double_decode;
mod t8_natural_eof_complete_playback;
