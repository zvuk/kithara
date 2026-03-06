#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

mod browser_runner_smoke;
mod env_guard;
mod events;
mod kithara_assets;
mod kithara_audio;
mod kithara_bufpool;

mod kithara_decode {
    pub(crate) mod fixture;

    mod decoder_seek_tests;
    mod decoder_tests;
    mod timeline_tests;
}

mod kithara_file {
    pub(crate) mod fixture;

    #[cfg(not(target_arch = "wasm32"))]
    mod early_stream_close;
    #[cfg(not(target_arch = "wasm32"))]
    mod file_source;
}

mod kithara_hls {
    pub(crate) mod fixture;

    mod abr_integration;
    mod basic_playback;
    mod deferred_abr;
    mod driver_test;
    mod ephemeral;
    mod keys_integration;
    mod playlist_integration;
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
mod timeout_guard;
