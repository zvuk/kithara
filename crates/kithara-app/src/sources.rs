use kithara::{hls::KeyOptions, prelude::ResourceConfig};
use kithara_queue::TrackSource;

use crate::config::AppConfig;

/// Build a [`TrackSource`] for `url`.
///
/// Attaches the shared [`Downloader`](kithara::stream::dl::Downloader)
/// and the app's [`KeyProcessorRegistry`](kithara_drm::KeyProcessorRegistry)
/// so DRM keys are routed to the correct processor by URL domain. Key
/// URLs that don't match any rule in the registry get the raw bytes
/// (e.g. silvercomet keys, which need no unwrap).
#[must_use]
pub fn build_source(url: &str, config: &AppConfig) -> TrackSource {
    match ResourceConfig::for_src(url) {
        Ok(builder) => {
            let keys = if config.key_registry.is_empty() {
                KeyOptions::default()
            } else {
                KeyOptions::builder()
                    .key_registry(config.key_registry.clone())
                    .build()
            };
            let cfg = builder
                .downloader(config.downloader.clone())
                .flush_hub(config.flush_hub.clone())
                .keys(keys)
                .build();
            TrackSource::Config(Box::new(cfg))
        }
        Err(e) => {
            tracing::error!(%url, %e, "failed to build ResourceConfig, falling back to Uri");
            TrackSource::Uri(url.to_string())
        }
    }
}

/// Build the full set of [`TrackSource`]s from `config.tracks`.
#[must_use]
pub fn build_sources(config: &AppConfig) -> Vec<TrackSource> {
    config
        .tracks
        .iter()
        .map(|url| build_source(url, config))
        .collect()
}
