#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

use kithara_test_dylib as _;

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}

mod basic_playback;
mod driver_test;
mod hls_seek_cancels_stale_fetches;
mod hls_seek_near_end_stress;
mod hls_variant_playlists_concurrent;
mod html_error_body;
mod playlist_integration;
mod prefetch_403_fails_open;
mod probe_not_ready_at_creation;
mod rapid_scrub_decode_failure;
mod seek_past_eof;
mod segment_boundary_strand;
mod size_probe_resolves_exact_offsets;
mod source_seek;
mod sync_reader_hls_test;
mod wait_range_contract;
