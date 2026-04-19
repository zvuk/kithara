#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

mod common;

#[cfg(not(target_arch = "wasm32"))]
#[path = "common/continuity.rs"]
pub(crate) mod continuity;

mod browser_runner_smoke;
mod env_guard;
mod events;
mod kithara_assets;
mod kithara_audio;
mod kithara_bufpool;

mod kithara_decode {
    mod decoder_seek_tests;
    mod decoder_tests;
    mod symphonia_seek_stale_duration;
    mod timeline_tests;
}

mod kithara_file {
    #[cfg(not(target_arch = "wasm32"))]
    mod early_stream_close;
    #[cfg(not(target_arch = "wasm32"))]
    mod file_source;
    mod html_error_cleanup;
}

mod kithara_hls {
    mod abr_integration;
    mod basic_playback;
    mod cancel_isolation;
    mod config_with_downloader;
    mod deferred_abr;
    mod driver_test;
    mod ephemeral;
    #[cfg(not(target_arch = "wasm32"))]
    mod html_error_body;
    mod html_error_cleanup;
    mod keys_integration;
    mod playlist_integration;
    mod red_leak_pattern;
    mod red_leak_peer_handle_cycle;
    mod red_leak_small_cache_seek;
    mod seek_past_eof;
    mod seek_variant_switch_after_eof;
    mod smoke_test;
    mod source_seek;
    mod sync_reader_hls_test;
    mod wait_range_contract;
}

mod kithara_net;
#[cfg(not(target_arch = "wasm32"))]
mod kithara_play;
mod kithara_storage;
mod kithara_stream;
mod thread_budget;
mod timeout_guard;
