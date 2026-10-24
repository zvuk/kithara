use std::{collections::HashMap, num::NonZeroUsize};

use kithara::{
    assets::StorageBackend,
    download::{Downloader, DownloaderConfig},
    drm::{KeyRequest, KeyRequestFactory},
    hls::KeyOptions,
    net::{HttpClient, NetOptions},
    platform::{CancelScope, sync::Arc},
    play::{
        PlayWorkerConfig, ResourceSrc,
        policy::{DomainKeyPolicy, DomainKeyRule},
    },
};

use crate::{
    observer::{AUTH_TOKEN_HEADER, SALT_HEADER},
    pools::{FfiResourceConfig, FfiStore, FfiTrackSource, FfiWorker, Pools},
    types::FfiKeyRule,
    web::keys,
};

pub(super) mod consts {
    pub(crate) const ASSET_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(128).unwrap();
    pub(crate) const ASSET_CACHE_MAX_BYTES: u64 = 128 * 1024 * 1024;
    use super::NonZeroUsize;
}

/// Main-thread resource preferences; each source captures its own immutable configuration.
#[derive(Clone)]
pub(in crate::web) struct ResourceBuilder {
    pub(super) store: FfiStore,
    pub(super) downloader: Downloader,
    pub(super) worker: FfiWorker,
    headers: HashMap<String, String>,
    keys: KeyOptions,
}

impl ResourceBuilder {
    pub(super) fn new(pools: Pools) -> Self {
        let scope = CancelScope::new(None);
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(
                NetOptions::default(),
                pools.clone(),
                scope.token().child(),
            ))
            .cancel(scope.token())
            .build(),
        );
        Self {
            downloader,
            store: FfiStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .cache_capacity(consts::ASSET_CACHE_CAPACITY)
                .max_bytes(consts::ASSET_CACHE_MAX_BYTES)
                .build(),
            worker: FfiWorker::new(PlayWorkerConfig::builder(pools).build()),
            headers: HashMap::new(),
            keys: KeyOptions::default(),
        }
    }

    pub(super) fn set_auth_token(&mut self, token: String) {
        if token.is_empty() {
            self.headers.remove(AUTH_TOKEN_HEADER);
        } else {
            self.headers.insert(AUTH_TOKEN_HEADER.to_owned(), token);
        }
    }

    pub(super) fn register_key_rule(&mut self, rule: FfiKeyRule, salt: String) {
        if let Some(headers) = rule.headers.as_ref() {
            self.headers.extend(headers.clone());
        }
        self.headers.insert(SALT_HEADER.to_owned(), salt.clone());
        let factory: KeyRequestFactory = Arc::new(move || {
            let mut headers = HashMap::new();
            headers.insert(SALT_HEADER.to_owned(), salt.clone());
            KeyRequest::new(headers, keys::worker_key_processor(salt.clone()))
        });
        let rule = DomainKeyRule::for_domains(&rule.domains, factory)
            .maybe_headers(rule.headers)
            .maybe_query_params(rule.query_params)
            .build();
        let mut registry = self.keys.key_registry.take().unwrap_or_default();
        registry.register(Arc::new(DomainKeyPolicy::new([rule])));
        self.keys = KeyOptions::builder().key_registry(registry).build();
    }

    pub(super) fn source(&self, url: String) -> Result<FfiTrackSource, String> {
        if self.keys.key_registry.is_none() && self.headers.is_empty() {
            return Ok(FfiTrackSource::Uri(url));
        }
        self.config(&url)
            .map(|config| FfiTrackSource::Config(Box::new(config)))
    }

    pub(super) fn config(&self, url: &str) -> Result<FfiResourceConfig, String> {
        let src = ResourceSrc::parse(url).map_err(|error| error.to_string())?;
        let headers = (!self.headers.is_empty()).then(|| self.headers.clone());
        Ok(FfiResourceConfig::for_src(src)
            .keys(self.keys.clone())
            .maybe_headers(headers.map(Into::into))
            .store(self.store.clone())
            .downloader(self.downloader.clone())
            .worker(self.worker.clone())
            .build())
    }
}
