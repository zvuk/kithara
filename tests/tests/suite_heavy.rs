#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

mod common;

pub use kithara_integration_tests::bufpool_ext;

#[cfg(not(target_arch = "wasm32"))]
#[path = "common/gapless.rs"]
mod gapless_common;

#[cfg(not(target_arch = "wasm32"))]
mod kithara_audio {
    mod alloc_free_hotpath;
}

mod kithara_decode {
    #[cfg(not(target_arch = "wasm32"))]
    mod fixture_integration;
    #[cfg(not(target_arch = "wasm32"))]
    mod gapless_encoding_parity;
    #[cfg(not(target_arch = "wasm32"))]
    mod gapless_parity;
    #[cfg(not(target_arch = "wasm32"))]
    mod hls_abr_variant_switch;
    mod stress_timeline;

    #[cfg(not(target_arch = "wasm32"))]
    mod stress_seek_random;
}

// The rest of this suite drives the local test server, the filesystem, and
// several players at once. The browser has none of that; `kithara_ffi_web` and
// `kithara_play::offline_browser` are what is meant to run there.
#[cfg(not(target_arch = "wasm32"))]
mod kithara_file {
    mod live_stress_real_mp3;
}

#[cfg(not(target_arch = "wasm32"))]
mod kithara_hls {
    mod deferred_abr_debug;
    mod drm_stream_integrity;
    mod stress_chunk_integrity;
    mod stress_seek_random;
}

mod kithara_ffi_web;

mod kithara_play {
    mod offline_browser;
}

#[cfg(not(target_arch = "wasm32"))]
mod multi_instance;

#[cfg(not(target_arch = "wasm32"))]
#[path = "kithara_play/no_sync_real_media.rs"]
mod no_sync_real_media;
