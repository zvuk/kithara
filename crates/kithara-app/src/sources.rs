use kithara::{hls::KeyOptions, net::Headers, prelude::ResourceConfig};
use kithara_queue::TrackSource;
use url::Url;

use crate::config::AppConfig;

/// Build a [`TrackSource`] for `url`.
///
/// Attaches the shared [`Downloader`](kithara::stream::dl::Downloader)
/// and the app's [`KeyProcessorRegistry`](kithara_drm::KeyProcessorRegistry)
/// so DRM keys are routed to the correct processor by URL domain. Key
/// URLs that don't match any rule in the registry get the raw bytes
/// (e.g. silvercomet keys, which need no unwrap).
///
/// When the track URL itself matches a registry rule (same host as the
/// DRM key), the rule's request headers are also forwarded to playlist
/// and segment fetches — providers that gate the playlist on the same
/// `X-Auth-Token` would otherwise redirect/403 before any key request
/// ever fires.
#[must_use]
pub fn build_source(url: &str, config: &AppConfig) -> TrackSource {
    build_resource_config(url, config).map_or_else(
        || TrackSource::Uri(url.to_string()),
        |cfg| TrackSource::Config(Box::new(cfg)),
    )
}

/// Build a [`ResourceConfig`] for `url` with the app's shared stores,
/// downloader, flush hub, and DRM routing injected (same as
/// [`build_source`]). `None` when `url` is not a valid source.
#[must_use]
pub(crate) fn build_resource_config(url: &str, config: &AppConfig) -> Option<ResourceConfig> {
    let builder = match ResourceConfig::for_src(url) {
        Ok(builder) => builder,
        Err(e) => {
            tracing::error!(%url, %e, "failed to build ResourceConfig");
            return None;
        }
    };
    let keys = if config.key_registry.is_empty() {
        KeyOptions::default()
    } else {
        KeyOptions::builder()
            .key_registry(config.key_registry.clone())
            .build()
    };
    let headers = Url::parse(url).ok().and_then(|parsed| {
        let host = parsed.host_str().unwrap_or("");
        let rule = config.key_registry.find(&parsed);
        rule.and_then(|r| r.headers.clone()).map_or_else(
            || {
                tracing::debug!(
                    %url,
                    host,
                    "drm: no registry rule for host -- plain (non-DRM) resource path"
                );
                None
            },
            |h| {
                let names: Vec<&String> = h.keys().collect();
                tracing::info!(
                    %url,
                    host,
                    header_names = ?names,
                    "drm: registry rule matched, forwarding headers to resource"
                );
                Some(Headers::from(h))
            },
        )
    });
    Some(
        builder
            .downloader(config.downloader.clone())
            .flush_hub(config.flush_hub.clone())
            .asset_store(config.asset_store.clone())
            .keys(keys)
            .maybe_headers(headers)
            .size_probe_method(config.size_probe_method)
            .build(),
    )
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
