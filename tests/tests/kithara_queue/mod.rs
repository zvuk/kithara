#[cfg(not(target_arch = "wasm32"))]
#[path = "../common/offline_player_harness.rs"]
mod offline_player_harness;

#[cfg(not(target_arch = "wasm32"))]
use kithara::{
    assets::StoreOptions,
    decode::DecoderBackend,
    hls::{AbrMode, KeyOptions},
    net::Headers,
    play::ResourceConfig,
    queue::TrackSource,
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_app::config::AppConfig;
#[cfg(not(target_arch = "wasm32"))]
use url::Url;

#[cfg(not(target_arch = "wasm32"))]
fn app_track_source(
    url: &str,
    config: &AppConfig,
    store: StoreOptions,
    backend: DecoderBackend,
    abr: AbrMode,
    name: Option<&str>,
) -> TrackSource {
    let Ok(builder) = ResourceConfig::for_src(url) else {
        return TrackSource::Uri(url.to_string());
    };
    let keys = if config.key_registry.is_empty() {
        KeyOptions::default()
    } else {
        KeyOptions::builder()
            .key_registry(config.key_registry.clone())
            .build()
    };
    let headers = Url::parse(url).ok().and_then(|parsed| {
        config
            .key_registry
            .find(&parsed)
            .and_then(|rule| rule.headers.clone())
            .map(Headers::from)
    });
    let builder = builder
        .downloader(config.downloader.clone())
        .flush_hub(config.flush_hub.clone())
        .byte_pool(config.byte_pool.clone())
        .asset_store(config.asset_store.clone())
        .keys(keys)
        .maybe_headers(headers)
        .size_probe_method(config.size_probe_method)
        .store(store)
        .decoder_backend(backend)
        .initial_abr_mode(abr);
    let cfg = match name {
        Some(name) => builder.name(name.to_owned()).build(),
        None => builder.build(),
    };
    TrackSource::Config(Box::new(cfg))
}

mod advance_boundary_provenance;
mod auto_advance;
mod cold_seek_middle;
mod cpal_cold_seek_synthetic;
mod early_seek_size_withheld_advance;
mod false_eof_rapid_scrub;
mod file_replay_from_warm_cache;
mod flac_swallow_fixture;
mod hls_seek_cancels_stale_fetches;
mod hls_seek_near_end_stress;
mod hls_variant_playlists_concurrent;
mod loader_starvation;
mod local_track_plays;
mod rapid_scrub_decode_failure;
mod real_playlist;
mod select_after_eof;
mod track_replay_after_switch;
mod track_switch_race;
mod user_simulation;
mod zvuk_cipher_check;
mod zvuk_drm_trace;
mod zvuk_prod_drm_e2e;
mod zvuk_prod_flac_swallow;
mod zvuk_stage_drm_e2e;
mod zvuk_stage_seed_brute_force;

// Mirror crate so the test binary can resolve `aes::cipher::*` directly.
// `cbc` already brings AES, but for ECB diagnostic we need the bare
// block cipher.
