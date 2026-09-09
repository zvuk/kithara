#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}

#[path = "aac_he_v2_hls_decode.rs"]
mod aac_he_v2_hls_decode;
#[path = "abr_integration.rs"]
mod abr_integration;
#[path = "basic_playback.rs"]
mod basic_playback;
#[path = "cancel_isolation.rs"]
mod cancel_isolation;
#[path = "config_with_downloader.rs"]
mod config_with_downloader;
#[path = "deferred_abr.rs"]
mod deferred_abr;
#[path = "driver_test.rs"]
mod driver_test;
#[path = "ephemeral.rs"]
mod ephemeral;
#[path = "forward_withheld_segment_busy_spin.rs"]
mod forward_withheld_segment_busy_spin;
#[path = "html_error_body.rs"]
mod html_error_body;
#[path = "html_error_cleanup.rs"]
mod html_error_cleanup;
#[path = "keys_integration.rs"]
mod keys_integration;
#[path = "playlist_integration.rs"]
mod playlist_integration;
#[path = "prefetch_403_fails_open.rs"]
mod prefetch_403_fails_open;
#[path = "probe_not_ready_at_creation.rs"]
mod probe_not_ready_at_creation;
#[path = "red_abr_no_escape_from_stalled_variant.rs"]
mod red_abr_no_escape_from_stalled_variant;
#[path = "red_leak_pattern.rs"]
mod red_leak_pattern;
#[path = "red_leak_peer_handle_cycle.rs"]
mod red_leak_peer_handle_cycle;
#[path = "red_leak_small_cache_seek.rs"]
mod red_leak_small_cache_seek;
#[path = "red_stale_tmp_claim_bricks_segment.rs"]
mod red_stale_tmp_claim_bricks_segment;
#[path = "seek_past_eof.rs"]
mod seek_past_eof;
#[path = "seek_variant_switch_after_eof.rs"]
mod seek_variant_switch_after_eof;
#[path = "segment_boundary_strand.rs"]
mod segment_boundary_strand;
#[path = "smoke_test.rs"]
mod smoke_test;
#[path = "source_seek.rs"]
mod source_seek;
#[path = "sync_reader_hls_test.rs"]
mod sync_reader_hls_test;
#[path = "wait_range_contract.rs"]
mod wait_range_contract;
