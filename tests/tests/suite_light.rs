#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

mod common;

#[path = "common/continuity.rs"]
pub(crate) mod continuity;
#[path = "common/gapless.rs"]
mod gapless_common;

mod browser_runner_smoke;
mod env_guard;
mod events;
mod flash_lexical;
mod kithara_assets;
mod kithara_audio;
mod kithara_bufpool;

#[cfg(not(target_arch = "wasm32"))]
mod kithara_encode {
    mod aac_tests;
    mod bytes_tests;
    mod error_tests;
    mod factory_tests;
    mod flac_tests;
    mod traits_tests;
    mod types_tests;
}

mod kithara_decode {
    mod aac_priming_regression;
    mod apple_mp3_priming_probe;
    mod decoder_seek_tests;
    mod decoder_tests;
    mod factory_tests;
    mod protocol_tests;
    mod symphonia_seek_stale_duration;
    mod symphonia_tests;
    mod timeline_tests;
}

mod kithara_app {
    #[cfg(not(target_arch = "wasm32"))]
    mod waveform_analyzer;
}

mod kithara_file {
    mod early_stream_close;
    mod file_source;
    mod html_error_cleanup;
    #[cfg(not(target_arch = "wasm32"))]
    mod shared_download;
    #[cfg(not(target_arch = "wasm32"))]
    mod waveform_shared_download;
}

mod kithara_hls {
    mod aac_he_v2_hls_decode;
    mod abr_integration;
    mod basic_playback;
    mod cancel_isolation;
    mod config_with_downloader;
    mod deferred_abr;
    mod driver_test;
    mod ephemeral;
    mod forward_withheld_segment_busy_spin;
    mod html_error_body;
    mod html_error_cleanup;
    mod keys_integration;
    mod playlist_integration;
    mod prefetch_403_fails_open;
    mod probe_not_ready_at_creation;
    mod red_abr_no_escape_from_stalled_variant;
    mod red_leak_pattern;
    mod red_leak_peer_handle_cycle;
    mod red_leak_small_cache_seek;
    mod seek_past_eof;
    mod seek_variant_switch_after_eof;
    mod segment_boundary_strand;
    mod smoke_test;
    mod source_seek;
    mod sync_reader_hls_test;
    mod wait_range_contract;
}

mod kithara_abr_integration;
mod kithara_net;
mod kithara_play;
mod kithara_queue;
mod kithara_storage;
mod kithara_stream;
#[cfg(not(target_arch = "wasm32"))]
mod no_block;
mod thread_budget;
mod timeout_guard;
