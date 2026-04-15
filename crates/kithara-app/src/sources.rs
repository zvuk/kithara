use kithara::prelude::ResourceConfig;
use kithara_queue::TrackSource;
use url::Url;

use crate::{config::AppConfig, drm};

/// Whether the host portion of `url` matches one of the configured DRM
/// domains.
fn needs_drm(url: &str, drm_domains: &[String]) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| drm_domains.iter().any(|d| h.ends_with(d.as_str())))
        })
        .unwrap_or(false)
}

/// Build a [`TrackSource`] for `url`, applying zvuk DRM keys when the host
/// matches `config.drm_domains`.
#[must_use]
pub fn build_source(url: &str, config: &AppConfig) -> TrackSource {
    if needs_drm(url, &config.drm_domains) {
        match ResourceConfig::new(url) {
            Ok(mut cfg) => {
                cfg = cfg.with_keys(drm::make_key_options());
                cfg.net.insecure = config.danger_accept_invalid_certs;
                TrackSource::Config(Box::new(cfg))
            }
            Err(e) => {
                tracing::error!(%url, %e, "failed to build DRM config, falling back to Uri");
                TrackSource::Uri(url.to_string())
            }
        }
    } else {
        TrackSource::Uri(url.to_string())
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
