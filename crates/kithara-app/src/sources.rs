use kithara::{hls::KeyOptions, prelude::ResourceConfig};
use kithara_queue::TrackSource;

use crate::{config::AppConfig, drm};

/// Build a [`TrackSource`] for `url`.
///
/// Attaches the shared [`Downloader`](kithara::stream::dl::Downloader)
/// and a domain-scoped [`KeyProcessorRegistry`] so DRM keys are routed
/// to the correct processor by URL domain. Non-DRM streams (e.g.
/// silvercomet) get raw keys — no processor applied.
#[must_use]
pub fn build_source(url: &str, config: &AppConfig) -> TrackSource {
    match ResourceConfig::new(url) {
        Ok(mut cfg) => {
            if !config.drm_domains.is_empty() {
                let registry = drm::make_key_registry(&config.drm_domains);
                cfg = cfg.with_keys(KeyOptions::new().with_key_registry(registry));
            }
            cfg = cfg.with_downloader(config.downloader.clone());
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
