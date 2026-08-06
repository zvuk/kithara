#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use bytes::Bytes;
use dashmap::DashMap;
use kithara_assets::{AssetResource, AssetScope, ReadSide, ResourceKey};
use kithara_drm::{DecryptContext, KeyProcessor, KeyProcessorRegistry, PreparedKeyRequest};
use kithara_events::{
    DrmEvent, EventBus, HlsError as EventHlsError, HlsEvent, KeyFailureStage, KeySource,
};
use kithara_net::Headers;
use kithara_platform::{sync::Arc, time::Instant};
use kithara_stream::dl::{FetchCmd, PeerHandle};
use url::Url;

use crate::{
    HlsError, HlsResult,
    handle::KeyPeer,
    logging::{RedactedNetError, RedactedUrl},
};

/// DRM key fetch + processor pipeline.
///
/// Resolves optional provider processing through a [`KeyProcessorRegistry`].
/// The registry supplies a fully prepared wire request; this type owns only
/// cache coordination, fetching, validation, and processor execution.
#[derive(Clone)]
pub struct KeyStore {
    keys: Arc<DashMap<Url, Bytes>>,
    scope: AssetScope,
    /// Byte buffer pool for reading cached key bodies.
    byte_pool: kithara_bufpool::BytePool,
    bus: EventBus,
    /// Cache-first + downloader pipeline for HLS-AES / DRM key bodies.
    key_peer: KeyPeer,
    /// Cache-wide headers (typically equal to `HlsConfig::headers`).
    base_headers: Option<Headers>,
    key_registry: Option<KeyProcessorRegistry>,
}

impl KeyStore {
    /// AES-128 key / IV length in bytes.
    const AES_KEY_LEN: usize = 16;

    /// Start offset for sequence number in the 16-byte IV.
    const IV_SEQUENCE_OFFSET: usize = 8;

    #[must_use]
    pub fn new(
        downloader: PeerHandle,
        scope: AssetScope,
        bus: EventBus,
        base_headers: Option<Headers>,
        key_registry: Option<KeyProcessorRegistry>,
        byte_pool: kithara_bufpool::BytePool,
    ) -> Self {
        Self {
            key_peer: KeyPeer::new(downloader, scope.clone(), byte_pool.clone()),
            scope,
            bus,
            base_headers,
            key_registry,
            byte_pool,
            keys: Arc::new(DashMap::new()),
        }
    }

    fn aes128_key_urls(media_playlists: &[crate::playlist::parse::MediaPlaylist]) -> HashSet<Url> {
        let mut urls: HashSet<Url> = HashSet::new();
        for playlist in media_playlists {
            for segment in &playlist.segments {
                if let Some(ref seg_key) = segment.key
                    && matches!(
                        seg_key.method,
                        crate::playlist::parse::EncryptionMethod::Aes128
                    )
                    && let Some(ref key_info) = seg_key.key_info
                    && let Ok(seg_url) = playlist.url.join(&segment.uri)
                    && let Ok(key_url) = resolve_key_url(key_info, &seg_url)
                {
                    urls.insert(key_url);
                }
            }
        }
        urls
    }

    async fn fetch_processed_body(
        &self,
        url: &Url,
        cmd: FetchCmd,
    ) -> HlsResult<(Bytes, Option<u64>)> {
        tracing::info!(url = %RedactedUrl::new(url), "drm key: fetching from network");
        let started_at = Instant::now();
        let resp = self.key_peer.execute(cmd).await.map_err(|e| {
            tracing::warn!(
                url = %RedactedUrl::new(url),
                error = %RedactedNetError::new(&e),
                "drm key: network fetch failed"
            );
            publish_key_fetch_failed(&self.bus, url, KeyFailureStage::Network, "key fetch failed");
            publish_decryption_error(&self.bus, "key fetch failed");
            HlsError::KeyProcessing("key fetch failed".to_string())
        })?;
        let latency_ms = u64::try_from(started_at.elapsed().as_millis()).ok();
        let raw_key = resp.body.collect().await.map_err(|e| {
            tracing::warn!(
                url = %RedactedUrl::new(url),
                error = %RedactedNetError::new(&e),
                "drm key: body collect failed"
            );
            publish_key_fetch_failed(
                &self.bus,
                url,
                KeyFailureStage::BodyCollect,
                "key body read failed",
            );
            publish_decryption_error(&self.bus, "key body read failed");
            HlsError::KeyProcessing("key body read failed".to_string())
        })?;
        Ok((raw_key, latency_ms))
    }

    async fn fetch_registered_key(
        &self,
        cache_key: &ResourceKey,
        url: &Url,
        registry: &KeyProcessorRegistry,
    ) -> HlsResult<Bytes> {
        if let Some(cached) = self.keys.get(url).map(|entry| entry.value().clone()) {
            publish_key_acquired(&self.bus, url, KeySource::MemCache, cached.len(), None);
            return Ok(cached);
        }

        if let Some(cached) = self.validated_key_cache(cache_key, url)? {
            return Ok(cached);
        }

        let Some(request) = registry.prepare(url) else {
            let headers = self.merged_headers(None);
            let key = self
                .key_peer
                .fetch_validated_in_transaction(cache_key, url, headers, validated_key_bytes)
                .await?;
            self.keys.insert(url.clone(), key.clone());
            return Ok(key);
        };
        let (cmd, processor) = self.prepared_request(request);
        let (raw_key, latency_ms) = self.fetch_processed_body(url, cmd).await?;
        let decrypted = self.process_key(url, &processor, raw_key)?;

        tracing::info!(
            url = %RedactedUrl::new(url),
            bytes = decrypted.len(),
            "drm key: fetched + decrypted, caching to asset store"
        );

        if let Err(error) = self.key_peer.write_back(cache_key, url, &decrypted) {
            tracing::warn!(
                url = %RedactedUrl::new(url),
                error = %error,
                "drm key: valid final key could not be persisted; using session memory"
            );
        }

        self.keys.insert(url.clone(), decrypted.clone());
        publish_key_acquired(
            &self.bus,
            url,
            KeySource::Network,
            decrypted.len(),
            latency_ms,
        );
        Ok(decrypted)
    }

    /// Synchronous key lookup — no network I/O.
    ///
    /// Keys normally come from the in-memory map populated by a prior
    /// [`Self::get_raw_key`] call on this session. The persistent
    /// [`AssetStore`](kithara_assets::AssetStore) remains a fallback for
    /// callers that have not prefetched.
    ///
    /// # Errors
    /// Returns an error when the key hasn't been fetched yet.
    pub fn get_cached_key(&self, url: &Url) -> HlsResult<Bytes> {
        if let Some(cached) = self.keys.get(url).map(|entry| entry.value().clone()) {
            publish_key_acquired(&self.bus, url, KeySource::MemCache, cached.len(), None);
            return Ok(cached);
        }

        let cache_key = self.scope.key(&AssetResource::Url(url.clone()))?;
        let res = self
            .scope
            .store()
            .open_resource(&cache_key, None)
            .map_err(|_e| {
                publish_key_fetch_failed(
                    &self.bus,
                    url,
                    KeyFailureStage::Missing,
                    "key not in cache",
                );
                HlsError::KeyProcessing("key not in cache".to_string())
            })?;
        let mut buf = self.byte_pool.get();
        let n = res.read_into(&mut buf).map_err(|_e| {
            publish_key_fetch_failed(
                &self.bus,
                url,
                KeyFailureStage::Missing,
                "cached key read failed",
            );
            HlsError::KeyProcessing("cached key read failed".to_string())
        })?;
        if n == 0 {
            publish_key_fetch_failed(
                &self.bus,
                url,
                KeyFailureStage::Missing,
                "cached key is empty",
            );
            return Err(HlsError::KeyProcessing("cached key is empty".to_string()));
        }
        let bytes = Bytes::copy_from_slice(&buf[..n]);
        if let Err(error) = validate_aes128_key(&bytes) {
            publish_key_fetch_failed(
                &self.bus,
                url,
                KeyFailureStage::Missing,
                "cached key has invalid length",
            );
            return Err(error);
        }
        self.keys.insert(url.clone(), bytes.clone());
        publish_key_acquired(&self.bus, url, KeySource::DiskCache, bytes.len(), None);
        Ok(bytes)
    }

    /// Load the final AES-128 key through an optional prepared request and processor.
    /// Cached bytes use the original URL identity and contain only processed key material.
    ///
    /// # Errors
    /// Returns an error when the fetch or the processor fails.
    ///
    pub async fn get_raw_key(
        &self,
        url: &Url,
        _iv: Option<[u8; Self::AES_KEY_LEN]>,
    ) -> HlsResult<Bytes> {
        if let Some(cached) = self.keys.get(url).map(|entry| entry.value().clone()) {
            tracing::debug!(url = %RedactedUrl::new(url), "key: served from in-memory cache");
            publish_key_acquired(&self.bus, url, KeySource::MemCache, cached.len(), None);
            return Ok(cached);
        }

        let cache_key = self.scope.key(&AssetResource::Url(url.clone()))?;
        let Some(registry) = self.key_registry.as_ref() else {
            let headers = self.merged_headers(None);
            let key = self
                .key_peer
                .fetch_validated(&cache_key, url, headers, validated_key_bytes)
                .await?;
            self.keys.insert(url.clone(), key.clone());
            return Ok(key);
        };
        let store = self.scope.store().clone();
        store
            .with_resource_transaction(&cache_key, || {
                self.fetch_registered_key(&cache_key, url, registry)
            })
            .await
    }

    /// Merge per-rule headers on top of base headers.
    /// Rule-specific entries take precedence on key conflict.
    fn merged_headers(&self, rule_headers: Option<&HashMap<String, String>>) -> Option<Headers> {
        let extra = rule_headers.cloned().map(Headers::from);
        match (self.base_headers.clone(), extra) {
            (None, None) => None,
            (Some(base), None) => Some(base),
            (None, Some(req)) => Some(req),
            (Some(base), Some(req)) => {
                let mut merged = base;
                for (k, v) in req.iter() {
                    merged.insert(k, v);
                }
                Some(merged)
            }
        }
    }

    /// Eagerly fetch every AES-128 key referenced by `media_playlists`,
    /// so [`Self::resolve_decrypt_ctx`] can serve them synchronously
    /// during variant construction. Cleartext playlists yield no
    /// fetches and the call resolves immediately.
    ///
    /// # Errors
    /// Returns the first [`HlsError`] from an underlying key fetch.
    pub(crate) async fn prefetch_aes128_keys(
        &self,
        media_playlists: &[crate::playlist::parse::MediaPlaylist],
    ) -> HlsResult<()> {
        let urls = Self::aes128_key_urls(media_playlists);
        let futs = urls
            .into_iter()
            .map(|url| async move { self.get_raw_key(&url, None).await.map(drop) });
        futures::future::try_join_all(futs).await?;
        Ok(())
    }

    fn prepared_request(&self, request: PreparedKeyRequest) -> (FetchCmd, KeyProcessor) {
        let headers = self.merged_headers(Some(&request.headers));
        let cmd = FetchCmd::get(request.url).maybe_headers(headers).build();
        (cmd, request.processor)
    }

    fn process_key(&self, url: &Url, processor: &KeyProcessor, raw_key: Bytes) -> HlsResult<Bytes> {
        let decrypted = processor(raw_key).map_err(|_error| {
            tracing::warn!(
                url = %RedactedUrl::new(url),
                "drm key: registry processor (decrypt) failed"
            );
            publish_key_fetch_failed(
                &self.bus,
                url,
                KeyFailureStage::Processor,
                "key processor failed",
            );
            publish_decryption_error(&self.bus, "key processor failed");
            HlsError::KeyProcessing("key processor failed".to_string())
        })?;
        if let Err(error) = validate_aes128_key(&decrypted) {
            tracing::warn!(
                url = %RedactedUrl::new(url),
                error = %error,
                "drm key: processed key has invalid length"
            );
            publish_key_fetch_failed(
                &self.bus,
                url,
                KeyFailureStage::Processor,
                "processed key has invalid length",
            );
            publish_decryption_error(&self.bus, "processed key has invalid length");
            return Err(error);
        }
        Ok(decrypted)
    }

    fn validated_key_cache(&self, cache_key: &ResourceKey, url: &Url) -> HlsResult<Option<Bytes>> {
        let Some(bytes) = self.key_peer.try_cached(cache_key, url)? else {
            return Ok(None);
        };
        if let Err(error) = validate_aes128_key(&bytes) {
            tracing::warn!(
                url = %RedactedUrl::new(url),
                error = %error,
                "drm key: cached final key is invalid; removing it before refetch"
            );
            self.scope.store().remove_resource(cache_key)?;
            return Ok(None);
        }
        tracing::info!(
            url = %RedactedUrl::new(url),
            bytes = bytes.len(),
            "drm key: served from disk cache"
        );
        self.keys.insert(url.clone(), bytes.clone());
        publish_key_acquired(&self.bus, url, KeySource::DiskCache, bytes.len(), None);
        Ok(Some(bytes))
    }

    /// Convenience constructor from [`crate::config::KeyOptions`].
    #[must_use]
    pub fn with_options(
        downloader: PeerHandle,
        scope: AssetScope,
        bus: EventBus,
        base_headers: Option<Headers>,
        options: crate::config::KeyOptions,
        byte_pool: kithara_bufpool::BytePool,
    ) -> Self {
        Self::new(
            downloader,
            scope,
            bus,
            base_headers,
            options.key_registry,
            byte_pool,
        )
    }
}

fn validate_aes128_key(bytes: &[u8]) -> HlsResult<()> {
    if bytes.len() == KeyStore::AES_KEY_LEN {
        return Ok(());
    }
    Err(invalid_key_length(bytes.len()))
}

fn validated_key_bytes(bytes: &[u8]) -> HlsResult<Bytes> {
    validate_aes128_key(bytes)?;
    Ok(Bytes::copy_from_slice(bytes))
}

fn invalid_key_length(actual: usize) -> HlsError {
    HlsError::KeyProcessing(format!(
        "AES-128 key must be {} bytes, got {actual}",
        KeyStore::AES_KEY_LEN
    ))
}

pub(crate) fn derive_iv(
    key_info: &crate::playlist::parse::KeyInfo,
    sequence: u64,
) -> [u8; KeyStore::AES_KEY_LEN] {
    if let Some(iv) = key_info.iv {
        return iv;
    }
    let mut iv = [0u8; KeyStore::AES_KEY_LEN];
    iv[KeyStore::IV_SEQUENCE_OFFSET..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

/// Build a [`DecryptContext`] from a pre-fetched key. Glues
/// `resolve_key_url` + `get_cached_key` + `derive_iv` into a single
/// sync step that runs after [`KeyStore::get_raw_key`] has populated the
/// cache. Caller invokes this only for AES-128 segments — the
/// presence of [`KeyInfo`](crate::playlist::parse::KeyInfo) means a key is
/// required, so every failure path here is a real error.
///
/// # Errors
/// - [`HlsError::InvalidUrl`] when the key URI cannot be resolved.
/// - [`HlsError::KeyProcessing`] when the cached key is missing,
///   empty, or not 16 bytes (AES-128 size).
pub(crate) fn resolve_decrypt_ctx(
    key_store: &KeyStore,
    key_info: &crate::playlist::parse::KeyInfo,
    segment_url: &Url,
    sequence: u64,
) -> HlsResult<DecryptContext> {
    let key_url = resolve_key_url(key_info, segment_url)?;
    let key_bytes = key_store.get_cached_key(&key_url)?;
    let key: [u8; KeyStore::AES_KEY_LEN] = key_bytes.as_ref().try_into().map_err(|_| {
        publish_key_fetch_failed(
            &key_store.bus,
            &key_url,
            KeyFailureStage::Missing,
            "cached key has invalid length",
        );
        invalid_key_length(key_bytes.len())
    })?;
    Ok(DecryptContext::new(key, derive_iv(key_info, sequence)))
}

/// Resolve the [`DecryptContext`] for a variant's init segment.
/// Returns `None` when the playlist has no init segment, when the
/// init segment carries no key (cleartext), or when key resolution
/// fails (same fallback policy as media segments).
pub(crate) fn resolve_init_decrypt_ctx(
    key_store: &KeyStore,
    playlist: &crate::playlist::parse::MediaPlaylist,
) -> Option<DecryptContext> {
    let init = playlist.init_segment.as_ref()?;
    let key = init.key.as_ref()?;
    if !matches!(key.method, crate::playlist::parse::EncryptionMethod::Aes128) {
        return None;
    }
    let key_info = key.key_info.as_ref()?;
    let init_url = playlist.url.join(&init.uri).ok()?;
    match resolve_decrypt_ctx(key_store, key_info, &init_url, 0) {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            tracing::warn!(
                url = %RedactedUrl::new(&init_url),
                error = %e,
                "resolve_init_decrypt_ctx failed; init will be unreadable",
            );
            None
        }
    }
}

pub(crate) fn resolve_key_url(
    key_info: &crate::playlist::parse::KeyInfo,
    segment_url: &Url,
) -> HlsResult<Url> {
    let key_uri = key_info
        .uri
        .as_ref()
        .ok_or_else(|| HlsError::InvalidUrl("missing key URI".to_string()))?;

    if key_uri.starts_with("http://") || key_uri.starts_with("https://") {
        Url::parse(key_uri).map_err(|e| HlsError::InvalidUrl(format!("Invalid key URL: {e}")))
    } else {
        segment_url
            .join(key_uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve key URL: {e}")))
    }
}

fn resolve_segment_decrypt_ctx(
    key_store: &KeyStore,
    media_url: &Url,
    segment: &crate::playlist::parse::MediaSegment,
) -> Option<DecryptContext> {
    let key = segment.key.as_ref()?;
    if !matches!(key.method, crate::playlist::parse::EncryptionMethod::Aes128) {
        return None;
    }
    let key_info = key.key_info.as_ref()?;
    let segment_url = media_url.join(&segment.uri).ok()?;
    match resolve_decrypt_ctx(key_store, key_info, &segment_url, segment.sequence) {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            tracing::warn!(
                url = %RedactedUrl::new(&segment_url),
                sequence = segment.sequence,
                error = %e,
                "resolve_decrypt_ctx failed; segment will be unreadable",
            );
            None
        }
    }
}

/// Resolve one [`DecryptContext`] per segment of `playlist`, using
/// keys already pre-fetched by [`KeyStore::prefetch_aes128_keys`].
/// Cleartext segments map to `None`; AES-128 segments map to
/// `Some(ctx)`. A resolution failure on an encrypted segment is
/// logged and the segment falls back to `None` — the dispatch path
/// will surface a decoder error, which is preferable to silently
/// shipping garbage bytes.
pub(crate) fn resolve_variant_decrypt_contexts(
    key_store: &KeyStore,
    playlist: &crate::playlist::parse::MediaPlaylist,
) -> Vec<Option<DecryptContext>> {
    playlist
        .segments
        .iter()
        .map(|segment| resolve_segment_decrypt_ctx(key_store, &playlist.url, segment))
        .collect()
}

fn publish_decryption_error(bus: &EventBus, detail: &str) {
    bus.publish(HlsEvent::Error {
        error: EventHlsError::Decryption(detail.to_string()),
    });
}

fn publish_key_acquired(
    bus: &EventBus,
    url: &Url,
    source: KeySource,
    bytes: usize,
    latency_ms: Option<u64>,
) {
    bus.publish(DrmEvent::KeyAcquired {
        source,
        bytes,
        latency_ms,
        key_host: key_host(url),
    });
}

fn publish_key_fetch_failed(bus: &EventBus, url: &Url, stage: KeyFailureStage, detail: &str) {
    bus.publish(DrmEvent::KeyFetchFailed {
        stage,
        key_host: key_host(url),
        detail: detail.to_string(),
    });
}

fn key_host(url: &Url) -> Option<String> {
    url.host_str().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        convert::Infallible,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Router,
        body::Body,
        extract::Query,
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    use bytes::Bytes;
    use kithara_assets::{
        AcquisitionResult, AssetResource, AssetScope, AssetSource, AssetStore, StorageBackend,
        WriteSide,
    };
    use kithara_drm::{
        DrmError, KeyProcessor, KeyProcessorRegistry, KeyRequest, KeyRequestFactory,
        KeyRequestResolver, PreparedKeyRequest,
    };
    use kithara_events::{DrmEvent, Event, EventBus, KeyFailureStage, KeySource};
    use kithara_net::{HttpClient, NetOptions};
    use kithara_platform::{
        CancelToken,
        sync::{Arc, Notify},
        tokio::{join, net::TcpListener as TokioTcpListener, task::spawn as tokio_spawn},
    };
    use kithara_stream::dl::{Downloader, DownloaderConfig, Peer};
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::*;
    use crate::playlist::parse::parse_media_playlist;

    const VALID_KEY: &[u8] = b"0123456789abcdef";

    struct MockPeer;

    impl kithara_abr::Abr for MockPeer {}
    impl Peer for MockPeer {}

    struct HostResolver {
        host: &'static str,
        request_factory: KeyRequestFactory,
    }

    impl KeyRequestResolver for HostResolver {
        fn prepare(&self, url: &Url) -> Option<PreparedKeyRequest> {
            if url.host_str() != Some(self.host) {
                return None;
            }
            let request = (self.request_factory)();
            Some(PreparedKeyRequest::new(
                url.clone(),
                request.headers,
                request.processor,
            ))
        }
    }

    struct FixedRequestResolver {
        host: &'static str,
        headers: HashMap<String, String>,
        processor: KeyProcessor,
        url: Url,
    }

    impl KeyRequestResolver for FixedRequestResolver {
        fn prepare(&self, url: &Url) -> Option<PreparedKeyRequest> {
            (url.host_str() == Some(self.host)).then(|| {
                PreparedKeyRequest::new(
                    self.url.clone(),
                    self.headers.clone(),
                    Arc::clone(&self.processor),
                )
            })
        }
    }

    fn const_factory(processor: KeyProcessor) -> KeyRequestFactory {
        Arc::new(move || KeyRequest::new(HashMap::new(), Arc::clone(&processor)))
    }

    fn registry_for(host: &'static str, processor: KeyProcessor) -> KeyProcessorRegistry {
        registry_for_factory(host, const_factory(processor))
    }

    fn registry_for_factory(
        host: &'static str,
        request_factory: KeyRequestFactory,
    ) -> KeyProcessorRegistry {
        let mut registry = KeyProcessorRegistry::new();
        registry.register(Arc::new(HostResolver {
            host,
            request_factory,
        }));
        registry
    }

    async fn spawn_key_server_with_body(body: Bytes) -> (Url, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let handler_requests = Arc::clone(&requests);
        let app = Router::new().route(
            "/key.bin",
            get(move || {
                let body = body.clone();
                let requests = Arc::clone(&handler_requests);
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    Result::<_, Infallible>::Ok(Body::from(body))
                }
            }),
        );
        let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio_spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let url = Url::parse(&format!("http://{addr}/key.bin")).expect("url");
        (url, requests)
    }

    async fn spawn_key_server() -> Url {
        spawn_key_server_with_body(Bytes::from_static(VALID_KEY))
            .await
            .0
    }

    async fn spawn_domain_key_server() -> (Url, Url) {
        let reversed = Bytes::from(VALID_KEY.iter().rev().copied().collect::<Vec<_>>());
        let masked = Bytes::from(VALID_KEY.iter().map(|byte| byte ^ 0xaa).collect::<Vec<_>>());
        let app = Router::new()
            .route(
                "/reversed.key",
                get(move || {
                    let body = reversed.clone();
                    async move { Result::<_, Infallible>::Ok(Body::from(body)) }
                }),
            )
            .route(
                "/masked.key",
                get(move || {
                    let body = masked.clone();
                    async move { Result::<_, Infallible>::Ok(Body::from(body)) }
                }),
            );
        let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        tokio_spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let localhost =
            Url::parse(&format!("http://localhost:{port}/reversed.key")).expect("localhost URL");
        let loopback =
            Url::parse(&format!("http://127.0.0.1:{port}/masked.key")).expect("loopback URL");
        (localhost, loopback)
    }

    async fn spawn_prepared_request_server() -> (Url, Url, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let handler_requests = Arc::clone(&requests);
        let reversed = Bytes::from(VALID_KEY.iter().rev().copied().collect::<Vec<_>>());
        let app = Router::new().route(
            "/wire.key",
            get(
                move |headers: HeaderMap, Query(query): Query<HashMap<String, String>>| {
                    let requests = Arc::clone(&handler_requests);
                    let reversed = reversed.clone();
                    async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        let authorized = headers
                            .get("X-Key-Auth")
                            .and_then(|value| value.to_str().ok())
                            == Some("authorized")
                            && query.get("session").map(String::as_str) == Some("fresh");
                        if authorized {
                            (StatusCode::OK, Body::from(reversed))
                        } else {
                            (StatusCode::UNAUTHORIZED, Body::empty())
                        }
                    }
                },
            ),
        );
        let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio_spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let original = Url::parse(&format!("http://{addr}/original.key")).expect("original URL");
        let wire = Url::parse(&format!("http://{addr}/wire.key?session=fresh")).expect("wire URL");
        (original, wire, requests)
    }

    async fn spawn_gated_key_server() -> (Url, Arc<AtomicUsize>, Arc<Notify>, Arc<Notify>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Notify::default());
        let release = Arc::new(Notify::default());
        let handler_requests = Arc::clone(&requests);
        let handler_seen = Arc::clone(&seen);
        let handler_release = Arc::clone(&release);
        let app = Router::new().route(
            "/key.bin",
            get(move || {
                let requests = Arc::clone(&handler_requests);
                let seen = Arc::clone(&handler_seen);
                let release = Arc::clone(&handler_release);
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    seen.notify_one();
                    release.notified().await;
                    Result::<_, Infallible>::Ok(Body::from(Bytes::from_static(VALID_KEY)))
                }
            }),
        );
        let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio_spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let url = Url::parse(&format!("http://{addr}/key.bin")).expect("url");
        (url, requests, seen, release)
    }

    fn collect_events(events: &mut kithara_events::EventReceiver) -> Vec<Event> {
        std::iter::from_fn(|| events.try_recv().ok())
            .map(|envelope| envelope.event)
            .collect()
    }

    fn make_store(
        bus: &EventBus,
        cancel: CancelToken,
        registry: Option<KeyProcessorRegistry>,
    ) -> KeyStore {
        let store = AssetStore::builder()
            .backend(StorageBackend::Memory)
            .cancel(cancel.clone())
            .build();
        make_store_with_assets(bus, cancel, registry, &store)
    }

    fn test_scope(store: &AssetStore) -> AssetScope {
        store
            .scope::<crate::Hls>(&AssetSource::Remote {
                url: Url::parse("https://example.com/master.m3u8").expect("master url"),
                discriminator: Some("key-test".to_owned()),
            })
            .expect("key asset scope")
    }

    fn make_store_with_assets(
        bus: &EventBus,
        network_cancel: CancelToken,
        registry: Option<KeyProcessorRegistry>,
        store: &AssetStore,
    ) -> KeyStore {
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), network_cancel))
                .build(),
        );
        let handle = downloader
            .register(Arc::new(MockPeer))
            .with_bus(bus.clone());
        KeyStore::new(
            handle,
            test_scope(store),
            bus.clone(),
            None,
            registry,
            kithara_bufpool::BytePool::default(),
        )
    }

    fn commit_key(store: &AssetStore, url: &Url, bytes: &[u8]) {
        let key = test_scope(store)
            .key(&AssetResource::Url(url.clone()))
            .expect("key resource path");
        let AcquisitionResult::Pending(writer) =
            store.acquire_resource(&key, None).expect("acquire key")
        else {
            panic!("fresh key must be pending");
        };
        writer.write_at(0, bytes).expect("write key");
        writer.commit(Some(bytes.len() as u64)).expect("commit key");
    }

    async fn assert_failed_persistence_keeps_prefetched_key(
        registry: Option<KeyProcessorRegistry>,
    ) {
        let (url, requests, seen, release) = spawn_gated_key_server().await;
        let store_cancel = CancelToken::never();
        let store = AssetStore::builder()
            .backend(StorageBackend::Memory)
            .cancel(store_cancel.clone())
            .build();
        let bus = EventBus::new(8);
        let keys = make_store_with_assets(&bus, CancelToken::never(), registry, &store);
        let media_url = url.join("/media.m3u8").expect("media url");
        let playlist = parse_media_playlist(
            media_url,
            b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:10,\nseg.ts\n#EXT-X-ENDLIST\n",
        )
        .expect("media playlist");
        let task_keys = keys.clone();
        let task_playlist = playlist.clone();
        let prefetch = tokio_spawn(async move {
            task_keys
                .prefetch_aes128_keys(std::slice::from_ref(&task_playlist))
                .await
        });
        seen.notified().await;
        store_cancel.cancel();
        release.notify_one();

        prefetch
            .await
            .expect("prefetch task")
            .expect("network key remains usable");
        let contexts = resolve_variant_decrypt_contexts(&keys, &playlist);
        assert_eq!(contexts.len(), 1);
        assert!(contexts[0].is_some());
        assert_eq!(
            keys.get_cached_key(&url).expect("session key").as_ref(),
            VALID_KEY
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[kithara::test(tokio)]
    async fn key_fetch_success_publishes_network_acquired() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let registry = registry_for("127.0.0.1", Arc::new(Ok::<Bytes, DrmError>));
        let store = make_store(&bus, CancelToken::never(), Some(registry));
        let url = spawn_key_server().await;

        let key = store
            .get_raw_key(&url, None)
            .await
            .expect("key fetch succeeds");
        assert_eq!(key.as_ref(), VALID_KEY);

        let events = collect_events(&mut events);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Drm(DrmEvent::KeyAcquired {
                key_host: Some(host),
                source: KeySource::Network,
                bytes: 16,
                latency_ms: Some(_),
            }) if host == "127.0.0.1"
        )));
    }

    #[kithara::test(tokio)]
    async fn domain_rules_apply_distinct_key_processors_end_to_end() {
        let (localhost_url, loopback_url) = spawn_domain_key_server().await;
        let localhost_calls = Arc::new(AtomicUsize::new(0));
        let loopback_calls = Arc::new(AtomicUsize::new(0));

        let reverse_calls = Arc::clone(&localhost_calls);
        let mut registry = registry_for(
            "localhost",
            Arc::new(move |key| {
                reverse_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Bytes::from(key.iter().rev().copied().collect::<Vec<_>>()))
            }),
        );
        let mask_calls = Arc::clone(&loopback_calls);
        registry.register(Arc::new(HostResolver {
            host: "127.0.0.1",
            request_factory: const_factory(Arc::new(move |key| {
                mask_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Bytes::from(
                    key.iter().map(|byte| byte ^ 0xaa).collect::<Vec<_>>(),
                ))
            })),
        }));
        let store = make_store(&EventBus::new(8), CancelToken::never(), Some(registry));

        let localhost_key = store
            .get_raw_key(&localhost_url, None)
            .await
            .expect("localhost key");
        let loopback_key = store
            .get_raw_key(&loopback_url, None)
            .await
            .expect("loopback key");

        assert_eq!(localhost_key.as_ref(), VALID_KEY);
        assert_eq!(loopback_key.as_ref(), VALID_KEY);
        assert_eq!(localhost_calls.load(Ordering::SeqCst), 1);
        assert_eq!(loopback_calls.load(Ordering::SeqCst), 1);
    }

    #[kithara::test(tokio)]
    async fn prepared_request_rewrites_wire_url_and_forwards_headers() {
        let (original_url, wire_url, requests) = spawn_prepared_request_server().await;
        let mut registry = KeyProcessorRegistry::new();
        registry.register(Arc::new(FixedRequestResolver {
            headers: HashMap::from([("X-Key-Auth".to_string(), "authorized".to_string())]),
            host: "127.0.0.1",
            processor: Arc::new(|key| {
                Ok(Bytes::from(key.iter().rev().copied().collect::<Vec<_>>()))
            }),
            url: wire_url,
        }));
        let store = make_store(&EventBus::new(8), CancelToken::never(), Some(registry));

        let key = store
            .get_raw_key(&original_url, None)
            .await
            .expect("prepared request key");

        assert_eq!(key.as_ref(), VALID_KEY);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[kithara::test(tokio)]
    async fn persisted_final_key_does_not_prepare_fresh_request() {
        let (url, requests) = spawn_key_server_with_body(Bytes::from_static(VALID_KEY)).await;
        let dir = tempdir().expect("tempdir");
        let assets = AssetStore::builder()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .cancel(CancelToken::never())
            .build();
        let bus = EventBus::new(8);

        let first_prepares = Arc::new(AtomicUsize::new(0));
        let first_counter = Arc::clone(&first_prepares);
        let first_registry = registry_for_factory(
            "127.0.0.1",
            Arc::new(move || {
                first_counter.fetch_add(1, Ordering::SeqCst);
                KeyRequest::new(HashMap::new(), Arc::new(Ok::<Bytes, DrmError>))
            }),
        );
        let first =
            make_store_with_assets(&bus, CancelToken::never(), Some(first_registry), &assets);
        assert_eq!(
            first
                .get_raw_key(&url, None)
                .await
                .expect("network key")
                .as_ref(),
            VALID_KEY
        );
        assert_eq!(first_prepares.load(Ordering::SeqCst), 1);
        assets.checkpoint().expect("persist final key");
        drop(first);
        drop(assets);

        let reopened_prepares = Arc::new(AtomicUsize::new(0));
        let reopened_counter = Arc::clone(&reopened_prepares);
        let reopened_registry = registry_for_factory(
            "127.0.0.1",
            Arc::new(move || {
                reopened_counter.fetch_add(1, Ordering::SeqCst);
                KeyRequest::new(HashMap::new(), Arc::new(Ok::<Bytes, DrmError>))
            }),
        );
        let reopened_assets = AssetStore::builder()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .cancel(CancelToken::never())
            .build();
        let reopened = make_store_with_assets(
            &bus,
            CancelToken::never(),
            Some(reopened_registry),
            &reopened_assets,
        );

        assert_eq!(
            reopened
                .get_raw_key(&url, None)
                .await
                .expect("cached key")
                .as_ref(),
            VALID_KEY
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(reopened_prepares.load(Ordering::SeqCst), 0);
    }

    #[kithara::test(tokio)]
    async fn key_resolution_failure_publishes_hls_error() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let registry = registry_for(
            "127.0.0.1",
            Arc::new(|_bytes| Err(DrmError::KeyProcessing("fixture processor failed".into()))),
        );
        let store = make_store(&bus, CancelToken::never(), Some(registry));
        let url = spawn_key_server().await;

        let err = store
            .get_raw_key(&url, None)
            .await
            .expect_err("processor fails");
        assert!(
            matches!(err, HlsError::KeyProcessing(detail) if detail.contains("key processor failed"))
        );
        let events = collect_events(&mut events);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Drm(DrmEvent::KeyFetchFailed {
                key_host: Some(host),
                stage: KeyFailureStage::Processor,
                detail,
            }) if host == "127.0.0.1" && detail == "key processor failed"
        )));
        let hls_error = events
            .into_iter()
            .find(|event| matches!(event, Event::Hls(HlsEvent::Error { .. })));
        assert!(matches!(
            hls_error,
            Some(Event::Hls(HlsEvent::Error {
                error: EventHlsError::Decryption(detail),
            })) if detail == "key processor failed"
        ));
    }

    #[kithara::test(tokio)]
    async fn key_mem_cache_hit_publishes_mem_cache_acquired() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let prepares = Arc::new(AtomicUsize::new(0));
        let prepare_counter = Arc::clone(&prepares);
        let registry = registry_for_factory(
            "127.0.0.1",
            Arc::new(move || {
                prepare_counter.fetch_add(1, Ordering::SeqCst);
                KeyRequest::new(HashMap::new(), Arc::new(Ok::<Bytes, DrmError>))
            }),
        );
        let store = make_store(&bus, CancelToken::never(), Some(registry));
        let url = spawn_key_server().await;

        let _ = store
            .get_raw_key(&url, None)
            .await
            .expect("first fetch succeeds");
        assert_eq!(prepares.load(Ordering::SeqCst), 1);
        let _ = collect_events(&mut events);
        let key = store
            .get_raw_key(&url, None)
            .await
            .expect("mem cache hit succeeds");
        assert_eq!(key.as_ref(), VALID_KEY);
        assert_eq!(prepares.load(Ordering::SeqCst), 1);

        let events = collect_events(&mut events);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Drm(DrmEvent::KeyAcquired {
                key_host: Some(host),
                source: KeySource::MemCache,
                bytes: 16,
                latency_ms: None,
            }) if host == "127.0.0.1"
        )));
    }

    #[kithara::test(tokio)]
    async fn corrupt_persisted_plain_key_is_invalidated_and_refetched_once() {
        let (url, requests) = spawn_key_server_with_body(Bytes::from_static(VALID_KEY)).await;
        let dir = tempdir().expect("tempdir");
        let store = AssetStore::builder()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .cancel(CancelToken::never())
            .build();
        commit_key(&store, &url, b"corrupt");
        store.checkpoint().expect("persist corrupt key");
        drop(store);

        let bus = EventBus::new(8);
        let store = AssetStore::builder()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .cancel(CancelToken::never())
            .build();
        let keys = make_store_with_assets(&bus, CancelToken::never(), None, &store);
        let key = keys
            .get_raw_key(&url, None)
            .await
            .expect("corrupt key must be repaired");
        assert_eq!(key.as_ref(), VALID_KEY);
        assert_eq!(keys.get_cached_key(&url).expect("memory key"), key);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        store.checkpoint().expect("persist repaired key");
        drop(keys);
        drop(store);

        let reopened = AssetStore::builder()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .cancel(CancelToken::never())
            .build();
        let keys = make_store_with_assets(&bus, CancelToken::never(), None, &reopened);
        assert_eq!(
            keys.get_raw_key(&url, None)
                .await
                .expect("repaired key must persist")
                .as_ref(),
            VALID_KEY
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[kithara::test(tokio)]
    async fn processed_key_repair_is_serialized_across_key_stores() {
        let (url, requests) = spawn_key_server_with_body(Bytes::from_static(VALID_KEY)).await;
        let store = AssetStore::builder()
            .backend(StorageBackend::Memory)
            .cancel(CancelToken::never())
            .build();
        commit_key(&store, &url, b"corrupt");
        let bus = EventBus::new(16);
        let registry = registry_for("127.0.0.1", Arc::new(Ok::<Bytes, DrmError>));
        let first =
            make_store_with_assets(&bus, CancelToken::never(), Some(registry.clone()), &store);
        let second = make_store_with_assets(&bus, CancelToken::never(), Some(registry), &store);

        let (first, second) = join!(
            first.get_raw_key(&url, None),
            second.get_raw_key(&url, None)
        );

        assert_eq!(first.expect("first repaired key").as_ref(), VALID_KEY);
        assert_eq!(second.expect("second repaired key").as_ref(), VALID_KEY);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[kithara::test(tokio)]
    #[case(false)]
    #[case(true)]
    async fn failed_key_persistence_does_not_break_prefetch_resolution(#[case] processed: bool) {
        let registry =
            processed.then(|| registry_for("127.0.0.1", Arc::new(Ok::<Bytes, DrmError>)));
        assert_failed_persistence_keeps_prefetched_key(registry).await;
    }
}
