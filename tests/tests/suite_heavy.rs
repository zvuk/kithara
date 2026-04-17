#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

mod common;

#[cfg(not(target_arch = "wasm32"))]
mod kithara_audio {
    mod alloc_free_hotpath;
}

mod kithara_decode {
    mod fixture_integration;
    mod hls_abr_variant_switch;
    mod stress_timeline;

    #[cfg(not(target_arch = "wasm32"))]
    mod stress_seek_random;
}

mod kithara_file {
    mod live_stress_real_mp3;
}

mod kithara_hls {
    mod deferred_abr_debug;
    mod drm_stream_integrity;
    mod source_internal_cases;
    mod stress_chunk_integrity;
    mod stress_seek_random;
}

#[cfg(not(target_arch = "wasm32"))]
mod kithara_queue;

mod kithara_wasm;
mod multi_instance;
