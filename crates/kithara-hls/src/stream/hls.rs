#![forbid(unsafe_code)]

use std::sync::OnceLock;

use kithara_assets::{
    AssetLayoutRegistry, AssetSource, AssetStore, AssetStoreBuilder, BytePool, ResourceKey,
    StorageBackend, StoreOptions,
};
use kithara_events::{DeferredBus, EventBus, HlsError as EventHlsError, HlsEvent, VariantInfo};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancelScope, CancelToken,
    sync::{Arc, ThreadGate},
    tokio::sync::mpsc,
    traits::FromWithParams,
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
    peer::HlsPeer,
    playlist::{
        KeyStore, MasterPlaylist, MediaPlaylist, ParsedMaster, PlaylistCache, PlaylistState,
        load_variant_playlists, resolve_init_decrypt_ctx, resolve_variant_decrypt_contexts,
    },
    signal::SizeSignal,
    variant::{HlsVariant, PlanCtx, VariantParams},
};

/// Marker type for HLS streaming.
pub struct Hls;

fn effective_look_ahead_segments(config: &HlsConfig) -> Option<usize> {
    if config.store.backend != StorageBackend::Memory {
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

        let store = config
            .asset_store
            .clone()
            .unwrap_or_else(|| Arc::new(build_asset_store(&config, cancel.clone())));
        let source = AssetSource::Remote {
            url: config.url.clone(),
            discriminator: config.name.clone(),
        };
        let scope = store
            .scope::<Self>(&source)
            .map_err(crate::HlsError::from)?;
        let invalidation_guard = store.subscribe_eviction(Arc::from(scope.asset_root()), evict_tx);

        let byte_pool = config.pool.clone();

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

        let (master, media_playlists) = load_playlists(&stream_peer, &bus, &config).await?;

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
            bus.clone(),
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
        let signal = SizeSignal::new(Arc::new(ThreadGate::default()), Arc::new(OnceLock::new()));
        let emit = Arc::new(DeferredBus::new(bus.clone(), 256));

        let plan_ctx = PlanCtx {
            bus: bus.clone(),
            look_ahead_bytes,
            look_ahead_segments,
            master_cancel: cancel.clone(),
            scope: stream_peer.scope(),
            headers: config.headers.clone(),
            prefetch_budget: config.download_batch_size.max(1),
            seek_epoch: seek_obs.epoch(),
            signal: signal.clone(),
            size_probe_method: config.size_probe_method,
        };

        let variants: Vec<Arc<HlsVariant>> = media_playlists
            .iter()
            .enumerate()
            .map(|(idx, mp)| {
                let init_decrypt_ctx = resolve_init_decrypt_ctx(&key_store, mp);
                let decrypt_contexts = resolve_variant_decrypt_contexts(&key_store, mp);
                HlsVariant::try_build(
                    &playlist_state,
                    VariantParams {
                        init_decrypt_ctx,
                        variant_idx: idx,
                        seek_obs: Arc::clone(&seek_obs),
                        decrypt_contexts: &decrypt_contexts,
                        ctx: &plan_ctx,
                    },
                )
            })
            .collect::<crate::HlsResult<Vec<_>>>()?;
        let variants: Arc<[Arc<HlsVariant>]> = variants.into();

        let coord = Arc::new(HlsCoord::new(
            HlsCoordEnv {
                signal,
                cancel: cancel.clone(),
                scope: stream_peer.scope(),
                headers: config.headers.clone(),
                emit: Arc::clone(&emit),
            },
            playhead,
            seek,
            stream_peer.peer_handle().abr().clone(),
            Arc::clone(&variants),
            Arc::clone(&playlist_state),
        ));

        let mut source = HlsSource::new(Arc::clone(&coord), emit, stream_scope);

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
/// playlist (master order).
async fn load_playlists(
    stream_peer: &StreamPeer,
    bus: &EventBus,
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

    let master_playlist = MasterPlaylist::new(
        playlist_cache.clone(),
        &stream_peer.scope(),
        config.url.clone(),
    )
    .map_err(SourceError::from)?;
    let master_key = master_playlist.key().clone();
    let master = master_playlist.load().await.map_err(|err| {
        publish_playlist_error(bus, &err);
        SourceError::from(err)
    })?;

    let media_playlists = load_variant_playlists(
        &playlist_cache,
        &stream_peer.scope(),
        &config.url,
        &master_key,
        &master.variants,
    )
    .await
    .map_err(|err| {
        publish_playlist_error(bus, &err);
        SourceError::from(err)
    })?;

    Ok((master, media_playlists))
}

fn publish_playlist_error(bus: &EventBus, err: &crate::HlsError) {
    if let crate::HlsError::PlaylistParse(detail) = err {
        bus.publish(HlsEvent::Error {
            error: EventHlsError::Playlist(detail.clone()),
        });
    }
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

/// Build a private per-stream `AssetStore` (cache, flush hub; AES-128
/// decryption travels per-acquire as a `ProcessCtx`). The caller
/// subscribes its eviction channel via
/// [`AssetStore::subscribe_eviction`](kithara_assets::AssetStore::subscribe_eviction).
fn build_asset_store(config: &HlsConfig, cancel: CancelToken) -> AssetStore {
    AssetStoreBuilder::default()
        .cancel(cancel)
        .backend(config.store.backend.clone())
        .layouts(layout_registry(&config.store))
        .maybe_max_assets(config.store.max_assets)
        .maybe_max_bytes(config.store.max_bytes)
        .pool(config.pool.clone())
        .maybe_cache_capacity(config.store.cache_capacity)
        .maybe_flush_hub(config.store.flush_hub.clone())
        .build()
}

/// Build an app-wide shared HLS asset store.
///
/// Inject the result into every [`HlsConfig::asset_store`] that should
/// cooperate on a single cache. Per-stream eviction delivery is bound by
/// [`AssetStore::subscribe_eviction`].
#[must_use]
pub fn build_shared_asset_store(
    store: &StoreOptions,
    pool: BytePool,
    cancel: CancelToken,
) -> Arc<AssetStore> {
    Arc::new(
        AssetStoreBuilder::default()
            .cancel(cancel)
            .backend(store.backend.clone())
            .layouts(layout_registry(store))
            .maybe_max_assets(store.max_assets)
            .maybe_max_bytes(store.max_bytes)
            .pool(pool)
            .maybe_cache_capacity(store.cache_capacity)
            .maybe_flush_hub(store.flush_hub.clone())
            .build(),
    )
}

fn layout_registry(store: &StoreOptions) -> AssetLayoutRegistry {
    let mut layouts = AssetLayoutRegistry::default();
    if let Some(layout) = &store.layout {
        layouts.register::<Hls>(Arc::clone(layout));
    }
    layouts
}
