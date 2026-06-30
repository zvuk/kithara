#![forbid(unsafe_code)]

use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use kithara_assets::{
    AssetScope, AssetScopeDelegate, AssetStore, AssetStoreBuilder, BytePool, EvictConfig,
    OnInvalidatedFn, ProcessChunkFn, ResourceKey, StoreOptions,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_events::{EventBus, VariantInfo};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancelScope, CancelToken, sync::CondvarGate, tokio::sync::mpsc, traits::FromWithParams,
};
use kithara_stream::{
    Activity, PlayheadState, PlayheadWrite, SeekObserve, SeekState, SourceError, StreamType,
    dl::{Downloader, DownloaderConfig, Peer},
};

use super::{
    coord::{HlsCoord, HlsCoordEnv},
    source::HlsSource,
};
use crate::{
    config::HlsConfig,
    handle::StreamPeer,
    invalidation::{HlsInvalidationGuard, HlsInvalidationRegistry, HlsStore},
    naming::HlsAssetScopeDelegate,
    peer::HlsPeer,
    playlist::{
        KeyStore, MasterPlaylist, MediaPlaylist, ParsedMaster, PlaylistCache, PlaylistState,
        load_variant_playlists,
    },
    signal::SizeSignal,
    variant::{HlsVariant, PlanCtx, VariantParams},
};

/// Marker type for HLS streaming.
pub struct Hls;

fn eviction_callback(
    evict_tx: mpsc::UnboundedSender<ResourceKey>,
    next: Option<OnInvalidatedFn>,
) -> OnInvalidatedFn {
    Arc::new(move |key: &ResourceKey| {
        let _ = evict_tx.send(key.clone());
        if let Some(ref callback) = next {
            callback(key);
        }
    })
}

fn effective_look_ahead_segments(config: &HlsConfig) -> Option<usize> {
    if !config.store.is_ephemeral {
        return None;
    }
    let capacity = config.store.cache_capacity?;
    let bounded = capacity
        .get()
        .saturating_sub(config.ephemeral_cache_non_media_reserve)
        .max(config.ephemeral_cache_min_media_window);
    Some(capacity.get().min(bounded))
}

impl StreamType for Hls {
    type Config = HlsConfig;
    type Events = EventBus;
    type Source = HlsSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        let naming: Arc<dyn AssetScopeDelegate> = Arc::new(HlsAssetScopeDelegate);
        let asset_root = naming.asset_root_for_url(&config.url, config.name.as_deref());
        let asset_root_arc: Arc<str> = Arc::from(asset_root.as_str());
        let stream_scope = CancelScope::new(config.cancel.clone());
        let cancel = stream_scope.token();

        let bus = config
            .bus
            .clone()
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        let downloader = config
            .downloader
            .clone()
            .unwrap_or_else(|| default_downloader(&config, &cancel));

        let (evict_tx, evict_rx) = mpsc::unbounded_channel::<ResourceKey>();
        // App-wide shared store: reuse the injected backend and register
        // this stream's eviction channel in the routing registry. Without
        // injection, build a private per-stream store whose
        // `on_invalidated` feeds this single eviction channel directly.
        let (scope, invalidation_guard) = if let Some(shared) = config.asset_store.as_ref() {
            shared_hls_scope(shared, asset_root_arc, naming, evict_tx)
        } else {
            let backend = build_asset_store(&config, cancel.clone(), evict_tx);
            (backend.scope_with_delegate(asset_root_arc, naming), None)
        };

        let byte_pool = config
            .pool
            .clone()
            .unwrap_or_else(|| BytePool::default().clone());

        let playhead = Arc::new(PlayheadState::new());
        let seek = Arc::new(SeekState::new());
        let seek_obs = Arc::clone(&seek) as Arc<dyn SeekObserve>;
        let hls_peer = Arc::new(HlsPeer::new(
            Arc::clone(&seek_obs),
            Arc::clone(&seek) as Arc<dyn Activity>,
            config.initial_abr_mode,
        ));
        let stream_peer = StreamPeer::register(
            &downloader,
            Arc::clone(&hls_peer) as Arc<dyn Peer>,
            bus.clone(),
            scope,
            byte_pool,
        );

        let (master, media_playlists) = load_playlists(&stream_peer, &config).await?;

        // Size the private per-stream LRU handle cache to the variant's
        // segment count when the caller left it at the default, so random
        // seeks reuse open segment resources instead of thrashing a tiny
        // window into repeated re-opens. The store owns the headroom/cap
        // policy; shared stores are app-wide and must not be resized here.
        if config.asset_store.is_none() && config.store.cache_capacity.is_none() {
            let max_variant_segments = media_playlists
                .iter()
                .map(|mp| mp.segments.len())
                .max()
                .unwrap_or(0);
            let installed = stream_peer
                .scope()
                .store()
                .reserve_cache_for(max_variant_segments);
            tracing::debug!(
                max_variant_segments,
                cache_capacity = installed.get(),
                "sized private per-stream handle cache"
            );
        }

        let key_store = KeyStore::with_options(
            stream_peer.peer_handle(),
            stream_peer.scope(),
            config.headers.clone(),
            config.keys.clone(),
            stream_peer.byte_pool(),
        );

        let playlist_state = Arc::new(PlaylistState::build(
            &master.variants[..],
            &media_playlists[..],
        ));

        let abr_variants: Vec<VariantInfo> = FromWithParams::build(&master, &media_playlists[..]);
        hls_peer.set_abr_variants(abr_variants);

        key_store
            .prefetch_aes128_keys(&media_playlists)
            .await
            .map_err(SourceError::from)?;
        let look_ahead_bytes = Some(
            config
                .look_ahead_bytes
                .unwrap_or(HlsConfig::DEFAULT_LOOK_AHEAD_BYTES),
        );
        let look_ahead_segments = effective_look_ahead_segments(&config);

        playhead.set_duration(playlist_state.track_duration());

        // Unified reader-wake handle: the shared readiness gate for the off-RT
        // `wait_range(_, None)` park (CONTEXT.md "Seek and wait_range Contract")
        // paired with the late-bound audio-worker wake. The wake is filled by
        // `HlsSource::set_worker_wake` once the worker exists; `SizeSignal::fire`
        // fires both on the two downloader write/settle sites so the RT decoder
        // re-ticks on data arrival, not on its 10 ms scheduler poll. Built once
        // here and cloned down into every consumer.
        let signal = SizeSignal::new(
            Arc::new(CondvarGate::<u64>::default()),
            Arc::new(OnceLock::new()),
        );

        let plan_ctx = PlanCtx {
            master_cancel: cancel.clone(),
            scope: stream_peer.scope(),
            headers: config.headers.clone(),
            prefetch_budget: config.download_batch_size.max(1),
            seek_epoch: seek_obs.epoch(),
            look_ahead_bytes,
            look_ahead_segments,
            signal: signal.clone(),
            size_probe_method: config.size_probe_method,
        };

        let variants: Vec<Arc<HlsVariant>> = media_playlists
            .iter()
            .enumerate()
            .map(|(idx, mp)| {
                let init_decrypt_ctx = key_store.resolve_init_decrypt_ctx(mp);
                let decrypt_contexts = key_store.resolve_variant_decrypt_contexts(mp);
                FromWithParams::build(
                    &playlist_state,
                    VariantParams {
                        variant_idx: idx,
                        seek_obs: Arc::clone(&seek_obs),
                        init_decrypt_ctx,
                        decrypt_contexts: &decrypt_contexts,
                        ctx: &plan_ctx,
                    },
                )
            })
            .collect();
        let variants = Arc::new(variants);

        let coord = Arc::new(HlsCoord::new(
            HlsCoordEnv {
                signal,
                cancel: cancel.clone(),
                scope: stream_peer.scope(),
                headers: config.headers.clone(),
            },
            playhead,
            seek,
            stream_peer.peer_handle().abr().clone(),
            Arc::clone(&variants),
            Arc::clone(&playlist_state),
        ));

        let mut source = HlsSource::new(Arc::clone(&coord), bus.clone(), stream_scope);

        hls_peer.activate(
            coord,
            evict_rx,
            config.download_batch_size.max(1),
            look_ahead_bytes,
            look_ahead_segments,
            config.size_probe_method,
        );

        source.set_peer_handle(stream_peer.peer_handle());
        source.set_hls_peer(hls_peer);
        source.set_invalidation_guard(invalidation_guard);

        Ok(source)
    }

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.bus.clone()
    }
}

/// Build the playlist cache and load the master plus every variant media
/// playlist (master order). Folds the general→specific playlist load out of
/// `Hls::create` so the master and media playlists arrive together.
async fn load_playlists(
    stream_peer: &StreamPeer,
    config: &HlsConfig,
) -> Result<(ParsedMaster, Vec<MediaPlaylist>), SourceError> {
    let playlist_cache = PlaylistCache::new(
        stream_peer.scope(),
        stream_peer.peer_handle(),
        stream_peer.byte_pool(),
    );
    playlist_cache.set_master_url(config.url.clone());
    playlist_cache.set_base_url(config.base_url.clone());
    playlist_cache.set_headers(config.headers.clone());

    let master = MasterPlaylist::new(
        playlist_cache.clone(),
        &stream_peer.scope(),
        config.url.clone(),
    )
    .load()
    .await?;

    let media_playlists = load_variant_playlists(
        &playlist_cache,
        &stream_peer.scope(),
        &config.url,
        &master.variants,
    )
    .await?;

    Ok((master, media_playlists))
}

/// Default transport when the caller injects none: a private `Downloader`
/// rooted at a child of the stream's cancel token.
fn default_downloader(config: &HlsConfig, cancel: &CancelToken) -> Downloader {
    let dl_cancel = cancel.child();
    let net_options: NetOptions =
        FromWithParams::build(config.net_options.clone(), config.pool.clone());
    let client = HttpClient::new(net_options, dl_cancel.child());
    let dl_config = DownloaderConfig::for_client(client)
        .cancel(dl_cancel)
        .build();
    Downloader::new(dl_config)
}

fn build_asset_store(
    config: &HlsConfig,
    cancel: CancelToken,
    evict_tx: mpsc::UnboundedSender<ResourceKey>,
) -> AssetStore<DecryptContext> {
    let drm_process_fn: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
            aes128_cbc_process_chunk(input, output, ctx, is_last)
        });
    let on_invalidated = eviction_callback(evict_tx, config.store.on_invalidated.clone());

    AssetStoreBuilder::default()
        .process_fn(drm_process_fn)
        .cancel(cancel)
        .on_invalidated(on_invalidated)
        .root_dir(&config.store.cache_dir)
        .evict_config(EvictConfig::from(&config.store))
        .ephemeral(config.store.is_ephemeral)
        .maybe_pool(config.pool.clone())
        .maybe_cache_capacity(config.store.cache_capacity)
        .maybe_flush_hub(config.store.flush_hub.clone())
        .build()
}

fn shared_hls_scope(
    shared: &HlsStore,
    asset_root: Arc<str>,
    naming: Arc<dyn AssetScopeDelegate>,
    evict_tx: mpsc::UnboundedSender<ResourceKey>,
) -> (AssetScope<DecryptContext>, Option<HlsInvalidationGuard>) {
    let guard = HlsInvalidationGuard::install(
        Arc::clone(&shared.registry),
        Arc::clone(&asset_root),
        evict_tx,
    );
    let scope = shared.backend.scope_with_delegate(asset_root, naming);
    (scope, Some(guard))
}

/// Build an app-wide shared HLS asset store: one
/// `AssetStore<DecryptContext>` (DRM `process_fn`, cache, flush hub) plus
/// a routing registry that steers per-`asset_root` invalidations to the
/// owning stream. Inject the result into every [`HlsConfig::asset_store`]
/// that should cooperate on a single cache. `cancel` must be a child of
/// the app master so a shutdown cascades through the store.
#[must_use]
pub fn build_shared_asset_store(
    store: &StoreOptions,
    pool: Option<BytePool>,
    cancel: CancelToken,
) -> HlsStore {
    let registry: HlsInvalidationRegistry = Arc::new(DashMap::new());
    let drm_process_fn: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
            aes128_cbc_process_chunk(input, output, ctx, is_last)
        });
    let on_invalidated =
        registry_eviction_callback(Arc::clone(&registry), store.on_invalidated.clone());

    let backend = AssetStoreBuilder::default()
        .process_fn(drm_process_fn)
        .cancel(cancel)
        .on_invalidated(on_invalidated)
        .root_dir(&store.cache_dir)
        .evict_config(EvictConfig::from(store))
        .ephemeral(store.is_ephemeral)
        .maybe_pool(pool)
        .maybe_cache_capacity(store.cache_capacity)
        .maybe_flush_hub(store.flush_hub.clone())
        .build();

    HlsStore {
        registry,
        backend: Arc::new(backend),
    }
}

/// Shared-store invalidation callback: route an evicted key to the live
/// stream that owns its `asset_root`, then chain any caller callback.
fn registry_eviction_callback(
    registry: HlsInvalidationRegistry,
    next: Option<OnInvalidatedFn>,
) -> OnInvalidatedFn {
    Arc::new(move |key: &ResourceKey| {
        if let Some(root) = key.asset_root()
            && let Some(tx) = registry.get(root)
        {
            let _ = tx.send(key.clone());
        }
        if let Some(ref callback) = next {
            callback(key);
        }
    })
}
