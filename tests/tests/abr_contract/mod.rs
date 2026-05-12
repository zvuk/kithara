//! ABR-switch contract tests T1..T8.
//!
//! Each test in this directory enforces a subset of the axioms
//! catalogued in `.docs/plans/2026-05-03-abr-switch-contract.md`.
//! Tests are independent: there are no shared assertion bundles that
//! would let a regression in one axiom mask a regression in another.

pub(crate) mod helpers;

mod probe_commit_sequence;
mod probe_pcm_seam_continuity;
mod probe_variant_dispatch;
mod probe_variant_rebuild;

mod t10_no_apply_format_change_until_v_new_init_committed;
mod t11_apply_format_change_target_offset_matches_init_range;
mod t12_read_at_offsets_contiguous_within_segment;
mod t13_next_frame_count_matches_segment_duration;
mod t14_post_apply_fetch_targets_current_decode_time_not_seg_0;
mod t1_phase_continuity_wave;
mod t2_exact_fetch_count;
mod t3_disk_equals_network;
mod t4_seek_freezes_abr;
mod t5_switch_latency_bounded;
mod t6_init_range_outcome_ok_seg0;
mod t7_no_double_decode;
mod t8_natural_eof_complete_playback;
mod t9_no_metadata_fallback_until_v_new_init_committed;
