//! HLS stream type implementation.
//!
//! Provides `Hls` marker type implementing `StreamType` trait.

use std::sync::{Arc, Mutex as StdMutex};

use kithara_assets::{
    AssetStore, AssetStoreBuilder, OnInvalidatedFn, ProcessChunkFn, ResourceKey, asset_root_for_url,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_events::{EventBus, HlsEvent};
use kithara_platform::time::Instant;
use kithara_stream::{
    StreamContext, StreamType, Timeline,
    dl::{Downloader, DownloaderConfig},
};

use crate::{
    HlsStreamContext,
    config::HlsConfig,
    coord::{HlsCoord, SegmentRequest},
    error::HlsError,
    loading::{KeyManager, PlaylistCache, SegmentLoader},
    parsing::variant_info_from_master,
    playlist::PlaylistState,
    scheduler::worker::spawn_hls_worker,
    source::{HlsSource, build_pair},
    stream_index::StreamIndex,
};

/// Marker type for HLS streaming.
pub struct Hls;

type InvalidationTarget = (Arc<kithara_platform::Mutex<StreamIndex>>, Arc<HlsCoord>);

fn make_invalidation_callback(
    target: Arc<StdMutex<Option<InvalidationTarget>>>,
    next: Option<OnInvalidatedFn>,
) -> OnInvalidatedFn {
    Arc::new(move |key: &ResourceKey| {
        if let Some((segments, coord)) = target
            .lock()
            .expect("HLS invalidation target lock poisoned")
            .as_ref()
            && segments.lock_sync().remove_resource(key)
        {
            coord.condvar.notify_all();
        }
        if let Some(ref callback) = next {
            callback(key);
        }
    })
}

impl StreamType for Hls {
    type Config = HlsConfig;
    type Coord = Arc<HlsCoord>;
    type Demand = SegmentRequest;
    type Source = HlsSource;
    type Error = HlsError;
    type Events = EventBus;

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.bus.clone()
    }

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        let asset_root = asset_root_for_url(&config.url, config.name.as_deref());
        let cancel = config.cancel.clone().unwrap_or_default();

        // Create event bus early so soft timeout callback can publish to it.
        let bus = config
            .bus
            .clone()
            .unwrap_or_else(|| EventBus::new(config.event_channel_capacity));

        // Unified downloader — sole HttpClient owner in production. Use a
        // child cancel token so dropping the private Downloader on Hls
        // teardown never propagates back up to the outer `cancel`.
        // See feedback_cancel_token_drop_in_tests.md for the rationale.
        let downloader = config.downloader.clone().unwrap_or_else(|| {
            let mut net_opts = config.net.clone();
            let slow_bus = bus.clone();
            net_opts.on_slow = Some(Arc::new(move || slow_bus.publish(HlsEvent::LoadSlow)));
            let mut dl_config = DownloaderConfig::default()
                .with_net(net_opts)
                .with_cancel(cancel.child_token());
            if let Some(handle) = config.runtime.clone() {
                dl_config = dl_config.with_runtime(handle);
            }
            if let Some(pool) = config.pool.clone() {
                dl_config = dl_config.with_pool(pool);
            }
            Downloader::new(dl_config)
        });

        // Build DRM process function for ProcessingAssets
        let drm_process_fn: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
                aes128_cbc_process_chunk(input, output, ctx, is_last)
            });
        let invalidation_target = Arc::new(StdMutex::new(None));
        let on_invalidated = make_invalidation_callback(
            Arc::clone(&invalidation_target),
            config.store.on_invalidated.clone(),
        );

        // Build storage backend with DRM processing support
        let mut builder = AssetStoreBuilder::new()
            .process_fn(drm_process_fn)
            .asset_root(Some(asset_root.as_str()))
            .cancel(cancel.clone())
            .on_invalidated(on_invalidated)
            .root_dir(&config.store.cache_dir)
            .evict_config(config.store.to_evict_config())
            .ephemeral(config.store.ephemeral);
        if let Some(ref pool) = config.pool {
            builder = builder.pool(pool.clone());
        }
        if let Some(cap) = config.store.cache_capacity {
            builder = builder.cache_capacity(cap);
        }
        let backend: AssetStore<DecryptContext> = builder.build();

        // Per-track handle for the SRP-decomposed HLS sub-systems below.
        // HLS does not yield commands through a `Stream<Item = FetchCmd>`
        // — every fetch is dispatched directly via `track.execute*()`.
        // `new_track()` creates the handle without registering a stream,
        // giving HLS a clean per-track cancel hierarchy without a noop
        // stream workaround.
        //
        // Each component below clones this handle so they all share the
        // same per-track state (id + cancellation token).
        let track = downloader.new_track();

        // Build the small SRP-decomposed HLS sub-systems directly. No
        // FetchManager façade.
        let playlist_cache = PlaylistCache::new(backend.clone(), track.clone());
        playlist_cache.set_master_url(config.url.clone());
        playlist_cache.set_base_url(config.base_url.clone());
        playlist_cache.set_headers(config.headers.clone());

        // KeyManager: own track + backend + headers, no FetchManager.
        let key_manager = Arc::new(KeyManager::from_options(
            track.clone(),
            backend.clone(),
            config.headers.clone(),
            config.keys.clone(),
        ));

        // SegmentLoader: own track + backend + headers, shares
        // PlaylistCache for media playlist lookups.
        let mut loader = SegmentLoader::new(
            track.clone(),
            backend.clone(),
            config.headers.clone(),
            playlist_cache.clone(),
        );
        loader.set_key_manager(key_manager);
        let loader = Arc::new(loader);

        // Load master playlist via PlaylistCache.
        let master = playlist_cache.master_playlist(&config.url).await?;

        // Load all media playlists eagerly for PlaylistState.
        let mut media_playlists = Vec::new();
        for variant in &master.variants {
            let media_url = playlist_cache.resolve_url(&config.url, &variant.uri)?;
            let playlist = playlist_cache
                .media_playlist(&media_url, crate::parsing::VariantId(variant.id.0))
                .await?;
            media_playlists.push((media_url, playlist));
        }

        let playlist_state = Arc::new(PlaylistState::from_parsed(
            &master.variants,
            &media_playlists,
        ));

        // Determine initial variant. When a shared ABR controller carries
        // stale state from a previous stream (e.g., current_variant=1 but
        // mode=Manual(0)), synchronize before reading the initial variant.
        // This ensures layout_variant matches the ABR target at startup.
        let initial_variant = config.abr.as_ref().map_or(0, |abr| {
            let now = Instant::now();
            let decision = abr.decide(now);
            if decision.changed {
                abr.apply(&decision, now);
            }
            abr.get_current_variant_index()
        });

        // Emit VariantsDiscovered event
        let variant_info = variant_info_from_master(&master);
        bus.publish(HlsEvent::VariantsDiscovered {
            variants: variant_info,
            initial_variant,
        });

        // Create HlsScheduler + HlsSource pair
        let (hls_downloader, mut source) = build_pair(
            backend,
            track.clone(),
            &master.variants,
            &config,
            Arc::clone(&playlist_state),
            bus,
        );
        *invalidation_target
            .lock()
            .expect("HLS invalidation target lock poisoned") =
            Some((Arc::clone(&source.segments), Arc::clone(&source.coord)));

        // Spawn the download worker (async task or dedicated thread).
        // The WorkerGuard is stored in HlsSource — dropping the source
        // cancels the worker via its child cancel token.
        let guard = spawn_hls_worker(
            hls_downloader,
            Arc::clone(&loader),
            &cancel,
            config.runtime.clone(),
        );
        source.set_worker(guard);

        Ok(source)
    }

    fn build_stream_context(source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
        Arc::new(HlsStreamContext::new(
            timeline,
            Arc::clone(&source.segments),
            Arc::clone(&source.coord.abr_variant_index),
        ))
    }
}
