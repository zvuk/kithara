#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

#[cfg(not(target_arch = "wasm32"))]
mod kithara_audio {
    mod alloc_free_hotpath;
}

mod kithara_decode {
    pub(crate) mod fixture;

    mod fixture_integration;
    mod hls_abr_variant_switch;
    mod stress_timeline;

    #[cfg(not(target_arch = "wasm32"))]
    mod stress_seek_random;
}

mod kithara_file {
    pub(crate) mod fixture;

    mod live_stress_real_mp3;
}

mod kithara_hls {
    pub(crate) mod fixture;

    mod deferred_abr_debug;
    mod live_stress_real_stream;
    mod source_internal_cases;
    mod stress_chunk_integrity;
    mod stress_seek_abr;
    mod stress_seek_abr_audio;
    mod stress_seek_audio;
    mod stress_seek_lifecycle;
    mod stress_seek_random;
}

mod kithara_wasm;
mod multi_instance;
