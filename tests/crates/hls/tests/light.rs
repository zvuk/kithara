#![cfg(test)]
#![forbid(unsafe_code)]

use kithara_test_dylib as _;

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}

mod abr_integration;
mod basic_playback;
mod cancel_isolation;
mod cold_seek_middle;
mod config_with_downloader;
mod cpal_cold_seek_synthetic;
mod deferred_abr;
mod driver_test;
mod ephemeral;
mod flac_swallow_fixture;
mod hls_seek_cancels_stale_fetches;
mod hls_seek_near_end_stress;
mod hls_variant_playlists_concurrent;
mod html_error_body;
mod html_error_cleanup;
mod playlist_integration;
mod prefetch_403_fails_open;
mod probe_not_ready_at_creation;
mod rapid_scrub_decode_failure;
mod red_abr_no_escape_from_stalled_variant;
mod red_leak_pattern;
mod red_leak_peer_handle_cycle;
mod red_leak_small_cache_seek;
mod red_stale_tmp_claim_bricks_segment;
mod seek_past_eof;
mod seek_variant_switch_after_eof;
mod segment_boundary_strand;
mod size_probe_resolves_exact_offsets;
mod source_seek;
mod sync_reader_hls_test;
mod wait_range_contract;
