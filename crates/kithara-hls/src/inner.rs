//! HLS stream type implementation.
//!
//! Provides `Hls` marker type implementing `StreamType` trait.

use std::{
    io,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};

use kithara_assets::{
    AssetStore, AssetStoreBuilder, OnInvalidatedFn, ProcessChunkFn, ResourceKey, asset_root_for_url,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_events::{EventBus, HlsEvent};
use kithara_net::NetError;
use kithara_platform::{Mutex, time::Instant};
use kithara_storage::ResourceExt;
use kithara_stream::{
    StreamContext, StreamType, Timeline,
    dl::{Downloader, DownloaderConfig, FetchCmd, OnCompleteFn, Peer, Priority, WriterFn},
};
use tracing::debug;

use crate::{ids::SegmentId, scheduler::HlsScheduler};

/// All mutable state behind a single Mutex.
struct HlsState {
    scheduler: HlsScheduler,
    loader: Arc<SegmentLoader>,
    waker: Option<Waker>,
    epoch_cancel: tokio_util::sync::CancellationToken,
}

/// HLS peer — one per track. Pre-init: `poll_next` returns Pending.
/// After `activate()`: Downloader drives segment downloads via
/// self-contained `FetchCmd` closures (`writer` + `on_complete`).
pub(crate) struct HlsPeer {
    state: Arc<Mutex<Option<HlsState>>>,
    /// Waker stored before activation (`poll_next` called but state is None).
    pending_waker: Mutex<Option<Waker>>,
    /// Cancels the waker-forwarding micro-task on drop.
    wake_cancel: tokio_util::sync::CancellationToken,
}

impl HlsPeer {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            pending_waker: Mutex::new(None),
            wake_cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub(crate) fn activate(self: &Arc<Self>, scheduler: HlsScheduler, loader: Arc<SegmentLoader>) {
        let reader_advanced = scheduler.coord.reader_advanced.clone();
        let cancel = scheduler.coord.cancel.clone();

        {
            let mut guard = self.state.lock_sync();
            *guard = Some(HlsState {
                scheduler,
                loader,
                waker: None,
                epoch_cancel: tokio_util::sync::CancellationToken::new(),
            });
        }

        // Wake pending waker from pre-activation poll_next calls.
        if let Some(waker) = self.pending_waker.lock_sync().take() {
            waker.wake();
        }

        // Waker forwarding: translate Notify → stored waker.
        let peer = Arc::clone(self);
        let wake_cancel = self.wake_cancel.clone();
        kithara_platform::tokio::task::spawn(async move {
            loop {
                kithara_platform::tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    () = wake_cancel.cancelled() => return,
                    () = reader_advanced.notified() => {
                        let guard = peer.state.lock_sync();
                        if let Some(ref state) = *guard
                            && let Some(waker) = state.waker.as_ref()
                        {
                            waker.wake_by_ref();
                        }
                    }
                }
            }
        });
    }
}

impl Peer for HlsPeer {
    fn priority(&self) -> Priority {
        Priority::Low
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "HLS scheduler poll_next is inherently complex"
    )]
    #[expect(clippy::significant_drop_tightening)]
    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        let mut guard = self.state.lock_sync();
        let Some(ref mut state) = *guard else {
            *self.pending_waker.lock_sync() = Some(cx.waker().clone());
            return Poll::Pending;
        };

        state.waker = Some(cx.waker().clone());
        let sched = &mut state.scheduler;

        debug!(
            download_variant = sched.download_variant,
            cursor = sched.current_segment_index(),
            "poll_next: entry"
        );

        // 1. Demand processing (seek or gap-fill).
        //    `demand_segment` tracks a same-epoch demand target so the
        //    batch loop can force re-download even if the segment
        //    appears cached (ephemeral LRU race: data evicted between
        //    poll_next check and reader read).
        //    `demand_variant_override` forces the batch loop to use the
        //    demanded variant when ABR has moved to a different one
        //    (layout gap-fill: reader on v0, ABR on v3).
        let mut demand_segment: Option<usize> = None;
        let mut demand_variant_override: Option<usize> = None;
        if let Some(req) = sched.next_valid_demand_request() {
            if req.seek_epoch != sched.active_seek_epoch {
                // New seek epoch — full reset.
                state.epoch_cancel.cancel();
                state.epoch_cancel = tokio_util::sync::CancellationToken::new();
                sched.demand_throttle_until = None;

                let (is_variant_switch, _is_midstream_switch) =
                    sched.classify_variant_transition(req.variant, req.segment_index);
                sched.handle_midstream_switch(_is_midstream_switch);
                if let Some(sizes) = sched.playlist_state.segment_sizes(req.variant) {
                    sched
                        .segments
                        .lock_sync()
                        .set_expected_sizes(req.variant, sizes);
                }
                if is_variant_switch {
                    sched.download_variant = req.variant;
                }
                let (cached_count, cached_end_offset) =
                    sched.populate_cached_segments_if_needed(req.variant);
                sched.apply_cached_segment_progress(req.variant, cached_count, cached_end_offset);

                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            // Same-epoch demand (read_at gap-fill or evicted segment
            // re-fetch). Rewind cursor to the demanded segment.
            demand_segment = Some(req.segment_index);
            if req.segment_index < sched.current_segment_index() {
                debug!(
                    variant = req.variant,
                    segment = req.segment_index,
                    cursor = sched.current_segment_index(),
                    "poll_next: same-epoch demand behind cursor, rewinding"
                );
                sched.rewind_current_segment_index(req.segment_index);
            }
            // If demand is for a different variant than the current
            // download_variant (ABR switched away from layout), override
            // the variant for THIS poll_next cycle to fill the gap.
            if req.variant != sched.download_variant {
                demand_variant_override = Some(req.variant);
            }
        }

        // 2. Flushing gate (prefetch only, demand bypasses above).
        if sched.coord.timeline().is_flushing() {
            debug!("poll_next: flushing, returning Pending");
            return Poll::Pending;
        }

        // 3. ABR decision.
        let old_variant = sched.abr.get_current_variant_index();
        let decision = sched.make_abr_decision();
        let mut variant = sched.abr.get_current_variant_index();

        // Demand variant override: reader needs data from layout variant
        // while ABR moved to a different one. Use the demanded variant
        // for this poll_next cycle so the batch loop fills the gap.
        if let Some(dv) = demand_variant_override
            && dv != variant
        {
            debug!(
                demand_variant = dv,
                abr_variant = variant,
                "poll_next: demand override — filling layout gap"
            );
            variant = dv;
        }

        // Layout gap-fill override: handle_tail_state previously redirected
        // download_variant to the layout variant to fill gaps. ABR must not
        // override it until the gap is filled, otherwise the batch loop
        // commits cached ABR segments and handle_tail_state resets the
        // cursor again (hot loop).
        //
        // An explicit reader demand takes precedence — the reader cannot
        // make progress without the demanded data, so overriding it back
        // to the gap-fill variant creates an infinite loop.
        if sched.filling_layout_gap
            && demand_variant_override.is_none()
            && sched.download_variant != variant
        {
            debug!(
                layout_variant = sched.download_variant,
                abr_variant = variant,
                "poll_next: continuing layout gap-fill"
            );
            variant = sched.download_variant;
        }

        let Some(num_segments) = sched.num_segments_for_plan(variant) else {
            debug!(variant, "poll_next: no num_segments, Pending");
            return Poll::Pending;
        };

        sched.publish_variant_applied(old_variant, variant, &decision);

        // 4. Tail check.
        if sched.handle_tail_state(variant, num_segments) {
            debug!(
                variant,
                num_segments,
                cursor = sched.current_segment_index(),
                reader_pos = sched.coord.timeline().byte_position(),
                eof = sched.coord.timeline().eof(),
                "poll_next: tail state, Pending"
            );
            return Poll::Pending;
        }

        // 5. Variant readiness (sync).
        let (is_variant_switch, is_midstream_switch) =
            sched.classify_variant_transition(variant, sched.current_segment_index());
        // Demand-driven variant override is a temporary gap-fill, not
        // a real ABR midstream switch.  handle_midstream_switch would
        // reset the cursor to the first missing segment of the OLD
        // variant, destroying the demand cursor position.
        if demand_variant_override.is_none() {
            sched.handle_midstream_switch(is_midstream_switch);
        }
        if is_variant_switch {
            sched.download_variant = variant;
        }
        if let Some(sizes) = sched.playlist_state.segment_sizes(variant) {
            sched
                .segments
                .lock_sync()
                .set_expected_sizes(variant, sizes);
        }
        let (cached_count, cached_end_offset) = sched.populate_cached_segments_if_needed(variant);
        sched.apply_cached_segment_progress(variant, cached_count, cached_end_offset);

        // Demand cursor protection: variant readiness may advance the
        // cursor past the demand target via cached-segment pre-population
        // or midstream switch reset.  Restore cursor so the batch loop
        // starts at (or before) the demanded segment.
        if let Some(ds) = demand_segment
            && sched.current_segment_index() > ds
        {
            debug!(
                demand = ds,
                cursor = sched.current_segment_index(),
                "poll_next: demand cursor protection, resetting"
            );
            sched.reset_cursor(ds);
        }

        // 5b. Ephemeral demand throttle: after processing a demand,
        // cap how far the cursor can advance so prefetch doesn't evict
        // the demanded segment from the LRU cache.  The cap is lifted
        // on new-epoch seeks (line 154) or when the reader catches up.
        if let Some(ds) = demand_segment {
            let ahead = sched.look_ahead_segments.unwrap_or(sched.prefetch_count);
            sched.demand_throttle_until = Some(ds + ahead);
        }
        if demand_segment.is_none()
            && let Some(cap) = sched.demand_throttle_until
            && sched.current_segment_index() >= cap
        {
            return Poll::Pending;
        }

        // 6. Fill batch up to prefetch_count.
        let mut cmds = Vec::new();
        let seek_epoch = sched.coord.timeline().seek_epoch();
        let has_init = sched.variant_has_init(variant);
        let mut need_init = has_init
            && (sched.force_init_for_seek
                || sched.switch_needs_init(
                    variant,
                    sched.current_segment_index(),
                    is_variant_switch,
                ));

        for batch_i in 0..sched.prefetch_count {
            let seg_idx = sched.current_segment_index();
            if seg_idx >= num_segments {
                break;
            }

            // Skip cached — but never skip the demanded segment.
            // Reader demand means "I cannot read this segment right
            // now", so force re-download even if it looks cached
            // (ephemeral LRU may have evicted the data between
            // poll_next's check and the reader's read).
            let is_demanded = demand_segment == Some(seg_idx);
            let skipped = !is_demanded
                && sched.should_skip_planned_segment(
                    variant,
                    seg_idx,
                    is_midstream_switch,
                    if old_variant != variant {
                        Some(old_variant)
                    } else {
                        None
                    },
                );
            if skipped {
                debug!(
                    variant,
                    seg_idx,
                    batch_i,
                    cursor_after = sched.current_segment_index(),
                    "poll_next: skipped segment"
                );
                continue;
            }

            sched.bus.publish(HlsEvent::SegmentStart {
                variant,
                segment_index: seg_idx,
                byte_offset: sched.coord.timeline().download_position(),
            });

            let plan_need_init = has_init
                && crate::scheduler::helpers::should_request_init(
                    need_init,
                    SegmentId::Media(seg_idx),
                );
            if plan_need_init {
                sched.force_init_for_seek = false;
            }
            need_init = false;

            // Prepare segment (sync — playlists + keys pre-fetched).
            let prepared = match state.loader.prepare_media_sync(variant, seg_idx) {
                Ok(p) => p,
                Err(e) => {
                    sched.publish_download_error("prepare_media_sync", &e);
                    sched.advance_current_segment_index(seg_idx + 1);
                    continue;
                }
            };

            // Cached hit — commit directly.
            if let Some(cached_len) = prepared.cached_len {
                let init_meta = if plan_need_init {
                    state.loader.get_init_segment_cached(variant)
                } else {
                    None
                };
                let init_len = init_meta.as_ref().map_or(0, |m| m.len);
                let init_url = init_meta.map(|m| m.url);

                let meta = match state.loader.complete_media(&prepared, cached_len) {
                    Ok(m) => m,
                    Err(e) => {
                        sched.publish_download_error("complete_media cached", &e);
                        sched.advance_current_segment_index(seg_idx + 1);
                        continue;
                    }
                };

                let cursor_before = sched.current_segment_index();
                sched.commit_fetch_inline(
                    variant,
                    seg_idx,
                    seek_epoch,
                    &meta,
                    init_len,
                    init_url,
                    std::time::Duration::ZERO,
                );
                // Ensure cursor advances even if commit_fetch_inline
                // dropped the commit (stale epoch, cross-codec, etc.).
                sched.advance_current_segment_index(seg_idx + 1);
                debug!(
                    variant,
                    seg_idx,
                    batch_i,
                    cached_len,
                    cursor_before,
                    cursor_after = sched.current_segment_index(),
                    "poll_next: cached commit"
                );
                continue;
            }

            // Build self-contained FetchCmd for network download.
            let resource = prepared
                .resource
                .clone()
                .expect("non-cached PreparedMedia must have resource");

            let init_meta = if plan_need_init {
                state.loader.get_init_segment_cached(variant)
            } else {
                None
            };
            let init_len = init_meta.as_ref().map_or(0, |m| m.len);
            let init_url = init_meta.map(|m| m.url);
            let url = prepared.url.clone();

            // Writer closure: captures resource + offset.
            let offset = Arc::new(AtomicU64::new(0));
            let res_w = resource.clone();
            let off_w = Arc::clone(&offset);
            let writer: WriterFn = Box::new(move |data: &[u8]| {
                let pos = off_w.fetch_add(data.len() as u64, Ordering::Relaxed);
                res_w.write_at(pos, data).map_err(io::Error::other)
            });

            // on_complete closure: captures Arc<Mutex<HlsState>> + segment metadata.
            let state_arc = Arc::clone(&self.state);
            let start = Instant::now();
            let on_complete: OnCompleteFn = Box::new(
                move |bytes_written: u64, error: Option<&NetError>| {
                    if let Some(e) = error {
                        // "cannot write to committed resource" means another
                        // on_complete already committed this segment (race
                        // between parallel fetches after rewind). Not a real
                        // error — the segment is ready.
                        let is_already_committed =
                            e.to_string().contains("cannot write to committed resource");
                        let mut guard = state_arc.lock_sync();
                        let Some(ref mut st) = *guard else {
                            return;
                        };
                        if !is_already_committed {
                            debug!(variant, seg_idx, error = %e, "segment fetch failed, rewinding cursor");
                            st.scheduler.rewind_current_segment_index(seg_idx);
                        }
                        if let Some(waker) = st.waker.as_ref() {
                            waker.wake_by_ref();
                        }
                        return;
                    }
                    // Complete media OUTSIDE HlsState lock so DRM
                    // processing + invalidation callback don't race with
                    // commit_fetch_inline for the segments lock.
                    let loader = {
                        let guard = state_arc.lock_sync();
                        let Some(ref st) = *guard else {
                            return;
                        };
                        Arc::clone(&st.loader)
                    };
                    let meta = loader.complete_media(&prepared, bytes_written);
                    let mut guard = state_arc.lock_sync();
                    let Some(ref mut st) = *guard else {
                        return;
                    };
                    if let Ok(ref meta) = meta {
                        st.scheduler.commit_fetch_inline(
                            variant,
                            seg_idx,
                            seek_epoch,
                            meta,
                            init_len,
                            init_url.clone(),
                            start.elapsed(),
                        );
                    }
                    if let Some(waker) = st.waker.as_ref() {
                        waker.wake_by_ref();
                    }
                },
            );

            cmds.push(
                FetchCmd::get(url)
                    .cancel(Some(state.epoch_cancel.clone()))
                    .writer(writer)
                    .on_complete(on_complete),
            );

            sched.advance_current_segment_index(seg_idx + 1);

            // Stop the batch after the demanded segment so that
            // subsequent prefetch downloads don't evict it from an
            // ephemeral LRU cache before the reader can consume it.
            if is_demanded {
                break;
            }
        }

        if cmds.is_empty() {
            if sched.current_segment_index() >= num_segments {
                // Delegate to handle_tail_state which handles EOF,
                // missing-segment rewind, layout gap-fill, and
                // demand-driven wake. Peer stays alive for re-fetch
                // after seek or LRU eviction (lifetime = cancel token).
                if !sched.handle_tail_state(variant, num_segments) {
                    // Cursor was rewound (gap-fill or ABR reset) —
                    // wake immediately to build FetchCmds.
                    cx.waker().wake_by_ref();
                }
                return Poll::Pending;
            }
            // Cached segments committed, wake to re-poll.
            debug!(
                variant,
                cursor = sched.current_segment_index(),
                num_segments,
                "poll_next: all cached, re-polling"
            );
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        debug!(
            variant,
            count = cmds.len(),
            cursor = sched.current_segment_index(),
            "poll_next: returning {} FetchCmds",
            cmds.len()
        );
        Poll::Ready(Some(cmds))
    }
}

use crate::{
    HlsStreamContext,
    config::HlsConfig,
    coord::{HlsCoord, SegmentRequest},
    error::HlsError,
    loading::{KeyManager, PlaylistCache, SegmentLoader},
    parsing::variant_info_from_master,
    playlist::PlaylistState,
    source::{HlsSource, build_pair},
    stream_index::StreamIndex,
};

/// Marker type for HLS streaming.
pub struct Hls;

type InvalidationTarget = (Arc<Mutex<StreamIndex>>, Arc<HlsCoord>);

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

        // Register HlsPeer — inactive until activate().
        let hls_peer = Arc::new(HlsPeer::new());
        let peer_handle = downloader.register(Arc::clone(&hls_peer) as Arc<dyn Peer>);

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
        loader.set_key_manager(Arc::clone(&key_manager));
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

        // Pre-fetch init segments + DRM keys first so poll_next can resolve
        // everything synchronously and init bytes are available for bitrate estimation.
        prefetch_init_and_keys(&loader, &key_manager, &media_playlists).await;

        // Estimate segment sizes: BYTERANGE → init bitrate → HEAD fallback.
        crate::loading::size_estimation::estimate_size_maps(
            &peer_handle,
            &playlist_state,
            &loader,
            &media_playlists,
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

        // Activate the peer — Downloader starts driving poll_next.
        hls_peer.activate(hls_downloader, Arc::clone(&loader));
        source.set_peer_handle(peer_handle);

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

/// Pre-fetch init segments and DRM keys into [`AssetStore`] so that
/// the playback-phase state machine can resolve everything
/// synchronously (no `execute()` from `poll_next`).
async fn prefetch_init_and_keys(
    loader: &Arc<SegmentLoader>,
    key_manager: &Arc<KeyManager>,
    media_playlists: &[(url::Url, crate::parsing::MediaPlaylist)],
) {
    use std::collections::HashSet;

    use crate::parsing::EncryptionMethod;

    let num_variants = media_playlists.len();

    // Init segments — one per variant that has an init segment.
    let init_futs: Vec<_> = (0..num_variants)
        .filter(|&v| {
            media_playlists
                .get(v)
                .and_then(|(_, pl)| pl.init_segment.as_ref())
                .is_some()
        })
        .map(|variant| {
            let loader = Arc::clone(loader);
            async move { (variant, loader.load_init_segment(variant).await) }
        })
        .collect();

    // DRM keys — collect unique key URLs across all variants.
    let mut key_urls: HashSet<url::Url> = HashSet::new();
    for (media_url, playlist) in media_playlists {
        for segment in &playlist.segments {
            if let Some(ref seg_key) = segment.key
                && matches!(seg_key.method, EncryptionMethod::Aes128)
                && let Some(ref key_info) = seg_key.key_info
                && let Ok(seg_url) = media_url.join(&segment.uri)
                && let Ok(key_url) = KeyManager::resolve_key_url(key_info, &seg_url)
            {
                key_urls.insert(key_url);
            }
        }
    }

    let key_futs: Vec<_> = key_urls
        .into_iter()
        .map(|url| {
            let km = Arc::clone(key_manager);
            async move { (url.clone(), km.get_raw_key(&url, None).await) }
        })
        .collect();

    // Execute init + key fetches concurrently.
    let (init_results, key_results) = futures::future::join(
        futures::future::join_all(init_futs),
        futures::future::join_all(key_futs),
    )
    .await;

    for (variant, result) in init_results {
        if let Err(e) = result {
            tracing::warn!(variant, error = %e, "failed to pre-fetch init segment");
        }
    }
    for (url, result) in key_results {
        if let Err(e) = result {
            tracing::warn!(url = %url, error = %e, "failed to pre-fetch DRM key");
        }
    }
}
