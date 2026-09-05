#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioDecoderConfig, DecoderResamplerSettings},
    decode::DecoderBackend,
    hls::{AbrMode, KeyOptions},
    play::{PlaybackResamplerBackend, ResourceSrc},
};
use kithara_app::{
    config::AppConfig,
    pools::{AppResourceConfig, AppStore, AppTrackSource},
};
use kithara_integration_tests::offline::AppQueueFixture;
use url::Url;

pub(crate) fn app_disk_asset_store(config: &AppConfig, root: impl Into<PathBuf>) -> AppStore {
    AssetStore::builder(config.worker.pools().clone())
        .backend(StorageBackend::Disk { root: root.into() })
        .build()
}

pub(crate) fn app_drm_track_source(
    url: &str,
    ctx: &AppQueueFixture,
    backend: DecoderBackend,
) -> AppTrackSource {
    app_track_source(
        url,
        &ctx.config,
        app_disk_asset_store(&ctx.config, ctx.cache.path()),
        backend,
        AbrMode::Auto(None),
        None,
    )
}

pub(crate) fn app_track_source(
    url: &str,
    config: &AppConfig,
    store: AppStore,
    backend: DecoderBackend,
    abr: AbrMode,
    discriminator: Option<&str>,
) -> AppTrackSource {
    let Ok(src) = ResourceSrc::parse(url) else {
        return AppTrackSource::Uri(url.to_string());
    };
    let builder = AppResourceConfig::for_src(src);
    let registry = config.drm.registry();
    let keys = if registry.is_empty() {
        KeyOptions::default()
    } else {
        KeyOptions::builder().key_registry(registry.clone()).build()
    };
    let headers = Url::parse(url)
        .ok()
        .and_then(|parsed| config.drm.resource_headers(&parsed));
    let decoder_defaults = AudioDecoderConfig::builder()
        .resampler(
            DecoderResamplerSettings::builder()
                .backend(PlaybackResamplerBackend::default())
                .build(),
        )
        .build();
    let decoder = AudioDecoderConfig::builder()
        .backend(backend)
        .gapless_mode(decoder_defaults.gapless_mode())
        .maybe_resampler(decoder_defaults.resampler().cloned())
        .build();
    let builder = builder
        .downloader(config.downloader.clone())
        .worker(config.worker.clone())
        .keys(keys)
        .maybe_headers(headers)
        .size_probe_method(config.size_probe_method)
        .store(store)
        .decoder(decoder)
        .initial_abr_mode(abr);
    let config = match discriminator {
        Some(discriminator) => builder.discriminator(discriminator).build(),
        None => builder.build(),
    };
    AppTrackSource::Config(Box::new(config))
}
