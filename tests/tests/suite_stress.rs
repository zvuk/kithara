#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

mod common;

#[cfg(not(target_arch = "wasm32"))]
#[path = "common/continuity.rs"]
pub(crate) mod continuity;

mod abr_contract;

mod kithara_hls {
    mod abr_auto_switch;
    mod abr_mode_switch;
    mod abr_switch_playback;
    mod idle_behavior;
    mod live_stress_real_stream;
    mod red_flaky_small_cache_hot_refetch;
    mod red_leak_native_drm_seek_resume;
    mod startup_no_eager_size_probe_storm;
    mod stress_seek_abr;
    mod stress_seek_abr_audio;
    mod stress_seek_audio;
    mod stress_seek_lifecycle;
}

mod kithara_play {
    #[path = "../kithara_play/hls_seek_middle_stress_long.rs"]
    mod hls_seek_middle_stress_long;

    #[path = "../kithara_play/flac_realtime_player_continuity.rs"]
    mod flac_realtime_player_continuity;
}

mod phase_continuity;
