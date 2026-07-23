#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::AssetStore,
    audio::AudioDecoderConfig,
    decode::DecoderBackend,
    hls::{AbrMode, KeyOptions},
    net::Headers,
    play::ResourceConfig,
    queue::TrackSource,
};
use kithara_app::config::AppConfig;
use url::Url;

pub(crate) fn app_track_source(
    url: &str,
    config: &AppConfig,
    store: AssetStore,
    backend: DecoderBackend,
    abr: AbrMode,
    discriminator: Option<&str>,
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
    let decoder_defaults = kithara::play::default_resource_decoder_config();
    let decoder = AudioDecoderConfig::builder()
        .backend(backend)
        .gapless_mode(decoder_defaults.gapless_mode())
        .maybe_resampler(decoder_defaults.resampler().cloned())
        .build();
    let builder = builder
        .downloader(config.downloader.clone())
        .byte_pool(config.byte_pool.clone())
        .pcm_pool(config.pcm_pool.clone())
        .keys(keys)
        .maybe_headers(headers)
        .size_probe_method(config.size_probe_method)
        .store(store)
        .decoder(decoder)
        .initial_abr_mode(abr);
    let config = match discriminator {
        Some(discriminator) => builder.discriminator(discriminator.to_owned()).build(),
        None => builder.build(),
    };
    TrackSource::Config(Box::new(config))
}
