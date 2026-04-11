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
    dl::{Downloader, DownloaderConfig, Peer},
};

pub(crate) struct HlsPeer;

impl Peer for HlsPeer {}

use crate::{
    HlsStreamContext,
    config::HlsConfig,
    coord::{HlsCoord, SegmentRequest},
    error::HlsError,
    loading::{KeyManager, PlaylistCache, SegmentLoader},
    parsing::variant_info_from_master,
    playlist::{PlaylistState, VariantSizeMap},
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

        // Per-peer handle for the SRP-decomposed HLS sub-systems below.
        // HLS registers as a Peer so the downloader can query active
        // state for priority. Every fetch is dispatched via
        // `peer_handle.execute()` — no Stream<Item=FetchCmd> needed.
        let peer_handle = downloader.register(Arc::new(HlsPeer));

        // Build the small SRP-decomposed HLS sub-systems directly. No
        // FetchManager façade.
        let playlist_cache = PlaylistCache::new(backend.clone(), peer_handle.clone());
        playlist_cache.set_master_url(config.url.clone());
        playlist_cache.set_base_url(config.base_url.clone());
        playlist_cache.set_headers(config.headers.clone());

        // KeyManager: own peer_handle + backend + headers.
        let key_manager = Arc::new(KeyManager::from_options(
            peer_handle.clone(),
            backend.clone(),
            config.headers.clone(),
            config.keys.clone(),
        ));

        // SegmentLoader: own peer_handle + backend + headers, shares
        // PlaylistCache for media playlist lookups.
        let mut loader = SegmentLoader::new(
            peer_handle.clone(),
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

        // Determine initial variant BEFORE pre-fetch so we know which
        // variant to prioritise.
        let initial_variant = config.abr.as_ref().map_or(0, |abr| {
            let now = Instant::now();
            let decision = abr.decide(now);
            if decision.changed {
                abr.apply(&decision, now);
            }
            abr.get_current_variant_index()
        });

        // Pre-fetch size maps (HEAD) + init segments (GET) — one batch.
        prefetch_metadata(
            &peer_handle,
            &backend,
            &playlist_state,
            config.headers.as_ref(),
        )
        .await;

        // Emit VariantsDiscovered event
        let variant_info = variant_info_from_master(&master);
        bus.publish(HlsEvent::VariantsDiscovered {
            variants: variant_info,
            initial_variant,
        });

        // Create HlsScheduler + HlsSource pair
        let (hls_downloader, mut source) = build_pair(
            backend,
            peer_handle.clone(),
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

/// Pre-fetch size maps (HEAD) + init segments (GET) for all variants
/// in a single `batch()` call. One iteration to collect, one batch,
/// one iteration to distribute.
async fn prefetch_metadata(
    peer_handle: &kithara_stream::dl::PeerHandle,
    _backend: &AssetStore<DecryptContext>,
    playlist_state: &PlaylistState,
    headers: Option<&kithara_net::Headers>,
) {
    use kithara_stream::dl::{FetchCmd, FetchMethod};

    use crate::playlist::PlaylistAccess;

    struct VariantMeta {
        variant: usize,
        probe_start: usize,
        probe_has_init: bool,
        probe_num_segments: usize,
    }

    fn parse_content_length(resp: &kithara_stream::dl::FetchResponse) -> u64 {
        resp.headers
            .get("content-length")
            .or_else(|| resp.headers.get("Content-Length"))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    }

    let num_variants = playlist_state.num_variants();
    let mut cmds: Vec<FetchCmd> = Vec::new();
    let mut metas: Vec<VariantMeta> = Vec::new();

    // Phase 1: one loop — collect HEAD probes + init GETs.
    for variant in 0..num_variants {
        let num_segments = playlist_state.num_segments(variant).unwrap_or(0);
        let init_url = playlist_state.init_url(variant);
        let needs_size_map = !playlist_state.has_size_map(variant) && num_segments > 0;

        let probe_start = cmds.len();
        let mut probe_has_init = false;

        if needs_size_map {
            if let Some(ref url) = init_url {
                cmds.push(FetchCmd {
                    method: FetchMethod::Head,
                    url: url.clone(),
                    range: None,
                    headers: headers.cloned(),
                });
                probe_has_init = true;
            }
            for i in 0..num_segments {
                if let Some(url) = playlist_state.segment_url(variant, i) {
                    cmds.push(FetchCmd {
                        method: FetchMethod::Head,
                        url,
                        range: None,
                        headers: headers.cloned(),
                    });
                }
            }
        }

        metas.push(VariantMeta {
            variant,
            probe_start,
            probe_has_init,
            probe_num_segments: if needs_size_map { num_segments } else { 0 },
        });
    }

    if cmds.is_empty() {
        return;
    }

    // Phase 2: single batch — all HEAD probes in parallel.
    let results = peer_handle.batch(cmds).await;

    // Phase 3: distribute results → size maps.
    for meta in &metas {
        if meta.probe_num_segments > 0 {
            let mut idx = meta.probe_start;
            let init_size = if meta.probe_has_init {
                let s = results
                    .get(idx)
                    .and_then(|r| r.as_ref().ok())
                    .map_or(0, parse_content_length);
                idx += 1;
                s
            } else {
                0
            };

            let mut offsets = Vec::with_capacity(meta.probe_num_segments);
            let mut segment_sizes = Vec::with_capacity(meta.probe_num_segments);
            let mut cumulative = 0u64;

            for i in 0..meta.probe_num_segments {
                let media_len = results
                    .get(idx)
                    .and_then(|r| r.as_ref().ok())
                    .map_or(0, parse_content_length);
                idx += 1;
                let total_seg = if i == 0 {
                    init_size + media_len
                } else {
                    media_len
                };
                offsets.push(cumulative);
                segment_sizes.push(total_seg);
                cumulative += total_seg;
            }

            playlist_state.set_size_map(
                meta.variant,
                VariantSizeMap {
                    segment_sizes,
                    offsets,
                    total: cumulative,
                },
            );
        }
    }
}
