use std::collections::HashMap;

use bytes::Bytes;
use dashmap::DashMap;
use kithara::{
    abr::AbrMode,
    download::{Downloader, DownloaderConfig},
    drm::{KeyProcessor, KeyRequest, KeyRequestFactory},
    effects::eq::generate_log_spaced_bands,
    events::ScopeLabel,
    hls::{KeyOptions, KeyProcessorRegistry},
    host::HostOwned,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::{Arc, Mutex},
    },
    play::{
        InterruptionKind, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceSrc,
        policy::{DomainKeyPolicy, DomainKeyRule},
    },
    queue::{QueueConfig, QueueError, RepeatMode, Transition},
    warp::{StretchControls, WarpConfig},
};

use super::salt;
fn player_timestretch() -> Arc<StretchControls> {
    let controls = StretchControls::new(1.0);
    #[cfg(all(
        feature = "apple",
        target_vendor = "apple",
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    controls.set_keylock(true);
    controls
}

use crate::{
    EventBridge, Router,
    asset::FfiAssetStore,
    config::FfiPlayerConfig,
    item::AudioPlayerItem,
    observer::{AUTH_TOKEN_HEADER, FfiKeyProcessor, PlayerObserver, SALT_HEADER, SeekCallback},
    pools::{FfiQueue, FfiQueueControl, FfiResourceConfig, FfiTrackSource, FfiWorker},
    registry::ItemRegistry,
    types::{
        FfiAbrMode, FfiActionAtItemEnd, FfiCrossfadeSettings, FfiDuckingMode, FfiError, FfiKeyRule,
        FfiPlaybackOrder, FfiPlayerSnapshot, FfiPlayerStatus, FfiRepeatMode,
    },
};

fn build_processor_closure(processor: Arc<dyn FfiKeyProcessor>, salt: String) -> KeyProcessor {
    Arc::new(move |key: Bytes| {
        Ok(Bytes::from(
            processor.process_key(key.to_vec(), salt.clone()),
        ))
    })
}

/// Build the default `NetOptions`. The `dev` feature enables the
/// `insecure` flag for local test servers; release builds always
/// validate TLS.
fn default_net_options() -> NetOptions {
    const INSECURE: bool = cfg!(feature = "dev");
    NetOptions::builder().is_insecure(INSECURE).build()
}

/// A caller-provided salt remains fixed for legacy FFI compatibility.
fn build_processor_rule(rule: FfiKeyRule) -> DomainKeyRule {
    let processor = rule.processor;
    let salt_template = rule.salt.unwrap_or_default();
    let factory: KeyRequestFactory = Arc::new(move || {
        let salt = salt_template.clone();
        let mut headers = HashMap::new();
        headers.insert(SALT_HEADER.to_string(), salt.clone());
        let proc = build_processor_closure(Arc::clone(&processor), salt);
        KeyRequest::new(headers, proc)
    });
    DomainKeyRule::for_domains(&rule.domains, factory)
        .maybe_headers(rule.headers)
        .maybe_query_params(rule.query_params)
        .build()
}

/// Convert the FFI-level [`crate::types::FfiKeyOptions`] into the
/// initial registry + the player-wide header snapshot to expose to
/// outgoing HTTP requests.
fn build_initial_key_state(
    ffi: crate::types::FfiKeyOptions,
) -> (KeyOptions, HashMap<String, String>) {
    if ffi.rules.is_empty() {
        return (KeyOptions::default(), HashMap::new());
    }
    let mut registry = KeyProcessorRegistry::new();
    let mut player_headers: HashMap<String, String> = HashMap::new();
    let mut rules: Vec<DomainKeyRule> = Vec::with_capacity(ffi.rules.len());
    for r in ffi.rules {
        if let Some(headers) = r.headers.as_ref() {
            for (k, v) in headers {
                player_headers.insert(k.clone(), v.clone());
            }
        }
        if let Some(salt) = r.salt.as_ref() {
            player_headers.insert(SALT_HEADER.to_string(), salt.clone());
        }
        rules.push(build_processor_rule(r));
    }
    registry.register(Arc::new(DomainKeyPolicy::new(rules)));
    let key_options = KeyOptions::builder().key_registry(registry).build();
    (key_options, player_headers)
}

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct PeakBitrate {
    pub(crate) cellular_bps: f64,
    pub(crate) wifi_bps: f64,
}

impl PeakBitrate {
    /// Effective ABR cap, in bits/sec, derived from the configured
    /// `wifi_bps` and `cellular_bps` ceilings.
    /// Returns `None` when both are unset (`0.0`), letting ABR consider
    /// every variant. Saturates at [`u64::MAX`] for absurdly large
    /// inputs (real bitrates fit comfortably in `u64`, but `UniFFI`
    /// `f64` callers can pass anything).
    pub(crate) fn effective_cap(self) -> Option<u64> {
        const U64_MAX_AS_F64: f64 = 18_446_744_073_709_551_615.0;
        let limits = [self.wifi_bps, self.cellular_bps]
            .into_iter()
            .filter(|v| *v > 0.0);
        let cap = limits
            .reduce(f64::min)
            .filter(|v| v.is_finite() && *v > 0.0)?;
        if cap >= U64_MAX_AS_F64 {
            return Some(u64::MAX);
        }
        #[cfg_attr(
            all(),
            expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "non-negative finite bitrate clamped above; the cast is safe for any\
                          realistic peak bitrate"
            )
        )]
        let cap_u64 = cap.trunc() as u64;
        Some(cap_u64)
    }
}

/// Platform-selected engine behind the [`crate::player::AudioPlayer`]
/// facade. On native (Apple / Android) targets this is [`NativeInner`];
/// the wasm arm (`WasmInner`) lands in Wave 4 once the worker owns the
/// queue. Until then `AudioPlayer` itself is native-only, so this file
/// owns the only arm of the alias.
pub(crate) type Inner = NativeInner;

/// Native (Apple / Android) implementation of the FFI player. Owns the
/// queue, downloader, event bridge, and DRM key state. The
/// `AudioPlayer` facade delegates every public method here so the
/// `UniFFI` surface stays a thin shell over this engine.
pub(crate) struct NativeInner {
    /// Swift-owned items indexed by `TrackId`. Populated by `insert`,
    /// drained by `remove` / `remove_all_items`. Lets `items` return
    /// the same `AudioPlayerItem` instances that Swift handed in (preserves
    /// identity + active per-item observer wiring).
    items: Arc<Mutex<ItemRegistry>>,
    /// Rust-owned asset store shared by the queue and every item resource.
    store: Arc<FfiAssetStore>,
    /// Cancellation root for player-owned work; the shared store owns a
    /// separate scope.
    shutdown: CancelToken,
    /// Player-wide HTTP headers (e.g. `X-Encrypted-Key`,
    /// `X-Auth-Token`). Merged into per-item `headers` on insert.
    /// Item-supplied headers take precedence on key collision.
    player_headers: DashMap<String, String>,
    /// Shared downloader for every track created through this player.
    /// Pinned to `FFI_RUNTIME` so its async tasks land on a runtime that
    /// is always alive, independent of the caller thread (Swift /
    /// Kotlin callbacks run without an ambient tokio context).
    downloader: Downloader,
    queue: FfiQueueControl,
    queue_owner: HostOwned<FfiQueue>,
    event_bridge: Mutex<Option<EventBridge>>,
    /// Mutable [`KeyOptions`] — initialised from [`FfiPlayerConfig`]
    /// and extended at runtime by `setup_hls_aes`. Cloned per-item on
    /// insert (snapshot semantics: items already in the queue keep their
    /// original key registry).
    key_options: Mutex<KeyOptions>,
    observer: Mutex<Option<Arc<dyn PlayerObserver>>>,
    /// Bandwidth caps configured via `update_peak_bitrate`. Wifi value
    /// drives the ABR cap unless cellular is tighter; cellular is held
    /// for future network-state-aware switching.
    peak_bitrate: Mutex<PeakBitrate>,
}

impl NativeInner {
    pub(crate) fn new(config: FfiPlayerConfig) -> Result<Self, FfiError> {
        let FfiPlayerConfig {
            key_options,
            store,
            eq_band_count,
            auth_token,
            playing_rate,
            playback_order,
            action_at_item_end,
            crossfade_settings,
        } = config;
        let cancel = CancelToken::root();
        let pools = store.pools().clone();
        let worker = FfiWorker::new(
            PlayWorkerConfig::builder(pools.clone())
                .cancel(cancel.child())
                .build(),
        );
        let player_cancel = cancel.clone();
        let queue_store = store.handle().clone();
        let player_config = PlayerConfig::builder()
            .eq_layout(generate_log_spaced_bands(eq_band_count as usize))
            .warp(WarpConfig::builder().stretch(player_timestretch()).build())
            .cancel(player_cancel.child())
            .sample_rate(super::session::requested_sample_rate())
            .worker(worker)
            .build();
        let player = PlayerImpl::new(player_config);
        let queue_config = QueueConfig::builder()
            .player(player)
            .runtime(crate::FFI_RUNTIME.clone())
            .store(queue_store)
            .playback_order(playback_order.try_into()?)
            .action_at_item_end(action_at_item_end.try_into()?)
            .crossfade_settings(crossfade_settings.try_into()?)
            .build();
        let queue_owner = super::session::insert(FfiQueue::new(queue_config))
            .expect("INVARIANT: the process Host must accept a freshly allocated Queue");
        let queue = queue_owner.control().clone();
        let net = default_net_options();
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(net, pools, cancel.child()))
                .runtime(crate::FFI_RUNTIME.clone())
                .build(),
        );
        let (key_options, player_headers) = build_initial_key_state(key_options);
        let player_headers_map: DashMap<String, String> = player_headers.into_iter().collect();
        let inner = Self {
            downloader,
            store,
            queue_owner,
            queue,
            shutdown: cancel,
            key_options: Mutex::new(key_options),
            player_headers: player_headers_map,
            peak_bitrate: Mutex::default(),
            observer: Mutex::default(),
            event_bridge: Mutex::default(),
            items: Arc::new(Mutex::default()),
        };
        inner.setup_network(auth_token);
        inner.set_playing_rate(playing_rate);
        Ok(inner)
    }

    pub(crate) fn advance_to_next_item(&self) -> Result<(), FfiError> {
        self.queue
            .next(Transition::None)
            .map(|_| ())
            .map_err(|error| FfiError::Internal {
                description: error.to_string(),
            })
    }

    pub(crate) fn append(&self, item: &Arc<AudioPlayerItem>) -> Result<(), FfiError> {
        let source = build_source_for_item(self, item)?;
        let id = item.track_id();
        self.enqueue(item, || {
            self.queue
                .append_with_id(id, source)
                .map(|_| ())
                .map_err(|error| FfiError::Internal {
                    description: error.to_string(),
                })
        })
    }

    /// Registers `item` before `add` puts it into the queue: a load that
    /// fails at once reports its status while `add` is still returning, and
    /// the event bridge drops a status for an item it cannot find.
    fn enqueue(
        &self,
        item: &Arc<AudioPlayerItem>,
        add: impl FnOnce() -> Result<(), FfiError>,
    ) -> Result<(), FfiError> {
        let id = item.track_id();
        self.items.lock().insert(id, Arc::clone(item));
        if let Err(error) = add() {
            self.items.lock().remove(&id);
            return Err(error);
        }
        *item.inserted.lock() = true;
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn current_item(&self) -> Option<Arc<AudioPlayerItem>> {
        let entry = self.queue.current()?;
        self.items.lock().get(&entry.id).cloned()
    }

    pub(crate) fn eq_band_count(&self) -> u32 {
        let n = self.queue.eq_band_count();
        u32::try_from(n).unwrap_or_else(|_| {
            tracing::error!(eq_band_count = n, "BUG: EQ band count exceeds u32::MAX");
            0
        })
    }

    pub(crate) fn eq_gain(&self, band: u32) -> f32 {
        self.queue.eq_gain(band as usize).unwrap_or(0.0)
    }

    pub(crate) fn insert(
        &self,
        item: &Arc<AudioPlayerItem>,
        after: Option<&Arc<AudioPlayerItem>>,
    ) -> Result<(), FfiError> {
        let source = build_source_for_item(self, item)?;
        let id = item.track_id();
        let after_id = after.map(|i| i.track_id());

        self.enqueue(item, || {
            self.queue
                .insert_with_id(id, source, after_id)
                .map(|_| ())
                .map_err(|e| FfiError::InvalidArgument {
                    reason: e.to_string(),
                })
        })
    }

    pub(crate) fn item_count(&self) -> u32 {
        let len = self.queue.len();
        u32::try_from(len).unwrap_or_else(|_| {
            tracing::error!(queue_len = len, "BUG: queue length exceeds u32::MAX");
            0
        })
    }

    pub(crate) fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        let tracks = self.queue.tracks();
        let items = self.items.lock();
        tracks
            .iter()
            .filter_map(|t| items.get(&t.id).cloned())
            .collect()
    }

    pub(crate) fn notify_audio_route_changed(&self, reason: &str) -> Result<(), FfiError> {
        self.queue
            .notify_audio_route_changed(reason)
            .map_err(|err| match err {
                QueueError::Play(err) => FfiError::from(err),
                other => FfiError::Internal {
                    description: other.to_string(),
                },
            })
    }

    pub(crate) fn remove(&self, item: &AudioPlayerItem) -> Result<(), FfiError> {
        if !*item.inserted.lock() {
            return Err(FfiError::InvalidArgument {
                reason: format!("item {} not in queue", item.audio_id()),
            });
        }
        let id = item.track_id();
        self.queue
            .remove(id)
            .map_err(|e| FfiError::InvalidArgument {
                reason: e.to_string(),
            })?;
        self.items.lock().remove(&id);
        *item.inserted.lock() = false;
        Ok(())
    }

    pub(crate) fn remove_all_items(&self) {
        self.queue.clear();
        let mut items = self.items.lock();
        for (_, item) in items.drain() {
            *item.inserted.lock() = false;
        }
    }

    pub(crate) fn replace_item(
        &self,
        index: u32,
        item: &Arc<AudioPlayerItem>,
    ) -> Result<(), FfiError> {
        let idx = index as usize;
        let tracks = self.queue.tracks();
        let old = tracks.get(idx).ok_or_else(|| FfiError::InvalidArgument {
            reason: format!("item index {idx} out of range (len: {})", tracks.len()),
        })?;
        let old_id = old.id;

        let source = build_source_for_item(self, item)?;
        let after_for_insert = if idx == 0 {
            None
        } else {
            tracks.get(idx - 1).map(|e| e.id)
        };
        let new_id = item.track_id();
        self.queue
            .insert_with_id(new_id, source, after_for_insert)
            .map_err(|e| FfiError::InvalidArgument {
                reason: e.to_string(),
            })?;
        let _ = self.queue.remove(old_id);

        {
            let mut items = self.items.lock();
            items.remove(&old_id);
            items.insert(new_id, Arc::clone(item));
        }
        *item.inserted.lock() = true;
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn return_to_previous_item(&self) -> Result<(), FfiError> {
        self.queue
            .previous(Transition::None)
            .map(|_| ())
            .map_err(|error| FfiError::Internal {
                description: error.to_string(),
            })
    }

    pub(crate) fn seek(
        &self,
        to_seconds: f64,
        tolerance: Option<f64>,
        callback: &Arc<dyn SeekCallback>,
    ) {
        let _ = tolerance;
        match self.queue.seek(to_seconds) {
            Ok(_outcome) => callback.on_complete(true),
            Err(_) => callback.on_complete(false),
        }
    }

    pub(crate) fn select(
        &self,
        item: &AudioPlayerItem,
        transition: crate::types::FfiTransition,
    ) -> Result<(), FfiError> {
        self.queue
            .select(item.track_id(), transition.try_into()?)
            .map_err(|e| match e {
                QueueError::NotReady(_) => FfiError::NotReady,
                other => FfiError::Internal {
                    description: other.to_string(),
                },
            })
    }

    pub(crate) fn set_abr_mode(&self, mode: FfiAbrMode) {
        let Some(handle) = self.queue.current_abr_handle() else {
            return;
        };
        let abr_mode = match mode {
            FfiAbrMode::Auto => AbrMode::Auto(None),
            FfiAbrMode::Manual { variant_index } => AbrMode::manual(variant_index as usize),
        };
        if let Err(err) = handle.set_mode(abr_mode) {
            tracing::warn!(?err, "set_abr_mode rejected by ABR state");
        }
    }

    pub(crate) fn set_action_at_item_end(
        &self,
        action: FfiActionAtItemEnd,
    ) -> Result<(), FfiError> {
        self.queue.set_action_at_item_end(action.try_into()?);
        Ok(())
    }

    pub(crate) fn set_crossfade_settings(
        &self,
        settings: FfiCrossfadeSettings,
    ) -> Result<(), FfiError> {
        self.queue
            .set_crossfade_settings(settings.try_into()?)
            .map_err(FfiError::from)
    }

    pub(crate) fn set_ducking_mode(&self, mode: FfiDuckingMode) -> Result<(), FfiError> {
        self.queue
            .set_session_ducking(mode.into())
            .map_err(|err| match err {
                QueueError::Play(err) => FfiError::from(err),
                other => FfiError::Internal {
                    description: other.to_string(),
                },
            })
    }

    pub(crate) fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), FfiError> {
        self.queue
            .set_eq_gain(band as usize, gain_db)
            .map_err(FfiError::from)
    }

    pub(crate) fn set_observer(&self, observer: Arc<dyn PlayerObserver>) {
        let rx = self.queue.subscribe();

        let bridge = EventBridge::spawn(
            rx,
            Router::new(Arc::clone(&observer), Arc::clone(&self.items)),
            self.queue.clone(),
            CancelToken::never(),
        );

        let mut eb = self.event_bridge.lock();
        let mut obs = self.observer.lock();
        *eb = Some(bridge);
        *obs = Some(observer);
        drop(obs);
        drop(eb);
    }

    pub(crate) fn set_playback_order(&self, order: FfiPlaybackOrder) -> Result<(), FfiError> {
        self.queue.set_playback_order(order.try_into()?);
        Ok(())
    }

    pub(crate) fn set_repeat_mode(&self, mode: FfiRepeatMode) -> Result<(), FfiError> {
        let mode = RepeatMode::try_from(mode).map_err(|rejected| FfiError::InvalidArgument {
            reason: format!("repeat mode {rejected:?} is not supported"),
        })?;
        self.queue.set_repeat(mode);
        Ok(())
    }

    pub(crate) fn setup_hls_aes(&self, processor: Arc<dyn FfiKeyProcessor>) {
        let salt = salt::drm_lowercase_hex_salt();
        let mut rule_headers = HashMap::new();
        rule_headers.insert(SALT_HEADER.to_string(), salt.clone());
        let rule = FfiKeyRule {
            processor,
            headers: Some(rule_headers),
            query_params: None,
            domains: vec!["*".to_string()],
            salt: Some(salt),
        };
        self.setup_hls_aes_with_rule(rule);
    }

    pub(crate) fn setup_hls_aes_with_rule(&self, rule: FfiKeyRule) {
        if let Some(headers) = rule.headers.as_ref() {
            for (k, v) in headers {
                self.player_headers.insert(k.clone(), v.clone());
            }
        }
        if let Some(salt) = rule.salt.as_ref() {
            self.player_headers
                .insert(SALT_HEADER.to_string(), salt.clone());
        }

        let processor_rule = build_processor_rule(rule);
        let mut opts = self.key_options.lock();
        let mut registry = opts.key_registry.take().unwrap_or_default();
        registry.register(Arc::new(DomainKeyPolicy::new([processor_rule])));
        *opts = KeyOptions::builder().key_registry(registry).build();
    }

    pub(crate) fn setup_network(&self, auth_token: String) {
        if auth_token.is_empty() {
            self.player_headers.remove(AUTH_TOKEN_HEADER);
        } else {
            self.player_headers
                .insert(AUTH_TOKEN_HEADER.to_string(), auth_token);
        }
    }

    pub(crate) fn snapshot(&self) -> FfiPlayerSnapshot {
        let view = self.queue.playback_view();
        FfiPlayerSnapshot {
            status: FfiPlayerStatus::from(self.queue.status()),
            current_time: view.position,
            duration: view.duration,
            rate: self.queue.rate(),
            playing_rate: self.queue.default_rate(),
            volume: self.queue.volume(),
            is_muted: self.queue.is_muted(),
        }
    }

    pub(crate) fn stop(&self) {
        self.queue.pause();
        let _ = self.queue.seek(0.0);
    }

    pub(crate) fn update_peak_bitrate(&self, wifi_bps: f64, cellular_bps: f64) {
        let updated = PeakBitrate {
            cellular_bps,
            wifi_bps,
        };
        *self.peak_bitrate.lock() = updated;
        if let Some(handle) = self.queue.current_abr_handle() {
            handle.set_max_bandwidth_bps(updated.effective_cap());
        }
    }

    delegate::delegate! {
        to self.queue {
            #[expr($.into())]
            pub(crate) fn crossfade_settings(&self) -> FfiCrossfadeSettings;
            #[expr($.into())]
            pub(crate) fn playback_order(&self) -> FfiPlaybackOrder;
            #[expr($.into())]
            pub(crate) fn action_at_item_end(&self) -> FfiActionAtItemEnd;
            #[expr($.unwrap_or(0.0))]
            #[call(position_seconds)]
            pub(crate) fn current_time(&self) -> f64;
            pub(crate) fn is_muted(&self) -> bool;
            pub(crate) fn notify_interruption(&self, kind: InterruptionKind);
            pub(crate) fn pause(&self);
            pub(crate) fn play(&self);
            #[call(default_rate)]
            pub(crate) fn playing_rate(&self) -> f32;
            pub(crate) fn rate(&self) -> f32;
            #[expr($.into())]
            pub(crate) fn repeat_mode(&self) -> FfiRepeatMode;
            #[expr($.map_err(FfiError::from))]
            pub(crate) fn reset_eq(&self) -> Result<(), FfiError>;
            pub(crate) fn set_muted(&self, muted: bool);
            #[call(set_default_rate)]
            pub(crate) fn set_playing_rate(&self, rate: f32);
            pub(crate) fn set_volume(&self, volume: f32);
            pub(crate) fn volume(&self) -> f32;
        }
    }
}

/// Build an [`FfiTrackSource::Config`] from the item's fields. Also attaches
/// a scoped bus so the item's per-resource event bridge captures events
/// published during `Resource::new` (`VariantsDiscovered` fires
/// synchronously during stream open — a late subscriber would miss it).
fn build_source_for_item(
    inner: &NativeInner,
    item: &Arc<AudioPlayerItem>,
) -> Result<FfiTrackSource, FfiError> {
    let scoped = inner.queue.bus().scoped_labeled(ScopeLabel {
        track: Some(item.track_id()),
        ..ScopeLabel::default()
    });
    let abr_mode = item.abr_mode().map(|mode| match mode {
        FfiAbrMode::Auto => AbrMode::Auto(None),
        FfiAbrMode::Manual { variant_index } => AbrMode::manual(variant_index as usize),
    });
    let src = ResourceSrc::parse(item.url()).map_err(|e| FfiError::InvalidArgument {
        reason: e.to_string(),
    })?;
    let config = FfiResourceConfig::for_src(src)
        .preferred_peak_bitrate(item.preferred_peak_bitrate().max(0.0))
        .maybe_headers(merged_headers_for_item(inner, item).map(Into::into))
        .events(scoped.clone())
        .downloader(inner.downloader.clone())
        .store(inner.store.handle().clone())
        .keys(inner.key_options.lock().clone())
        .initial_abr_mode(abr_mode.unwrap_or_default())
        .build();
    *item.bus.lock() = Some(scoped);

    Ok(FfiTrackSource::Config(Box::new(config)))
}

/// Merge player-wide headers (auth, salt, …) into the item's own headers.
/// Item-supplied entries win on key collision so callers can override
/// player defaults per-item.
fn merged_headers_for_item(
    inner: &NativeInner,
    item: &Arc<AudioPlayerItem>,
) -> Option<HashMap<String, String>> {
    let item_headers = item.headers();
    if inner.player_headers.is_empty() && item_headers.is_none() {
        return None;
    }
    let mut merged: HashMap<String, String> = inner
        .player_headers
        .iter()
        .map(|r| (r.key().clone(), r.value().clone()))
        .collect();
    if let Some(item_h) = item_headers {
        merged.extend(item_h);
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

impl Drop for NativeInner {
    /// Fire the master cancel pulse so the shutdown signal reaches subsystems before
    /// structural Arc teardown unwinds. The facade owns `NativeInner` by value, so this
    /// runs exactly when the `AudioPlayer` is dropped.
    fn drop(&mut self) {
        if let Err(error) = super::session::remove(&self.queue_owner) {
            tracing::error!(?error, "failed to remove FFI Queue from the process Host");
        }
        self.shutdown.cancel();
    }
}

#[cfg(test)]
mod tests {
    use unimock::Unimock;

    use super::*;
    use crate::observer::FfiKeyProcessor;

    struct TaggedProcessor(u8);

    impl FfiKeyProcessor for TaggedProcessor {
        fn process_key(&self, _key: Vec<u8>, _salt: String) -> Vec<u8> {
            vec![self.0]
        }
    }

    fn tagged_rule(tag: u8, salt: &str, domains: &[&str]) -> FfiKeyRule {
        FfiKeyRule {
            processor: Arc::new(TaggedProcessor(tag)),
            headers: Some(HashMap::from([("X-Provider".to_string(), tag.to_string())])),
            query_params: None,
            domains: domains.iter().map(ToString::to_string).collect(),
            salt: Some(salt.to_string()),
        }
    }

    #[kithara::test]
    fn shared_store_outlives_each_player() {
        let store = Arc::new(FfiAssetStore::for_test());
        let cancel = store.cancel_token();
        let config = |store| FfiPlayerConfig {
            store,
            ..FfiPlayerConfig::for_test()
        };
        let first = NativeInner::new(config(Arc::clone(&store))).expect("create first player");
        let second = NativeInner::new(config(Arc::clone(&store))).expect("create second player");

        assert!(Arc::ptr_eq(&first.store, &second.store));
        assert!(first.store.handle().is_same(second.store.handle()));

        drop(store);
        drop(first);
        assert!(!cancel.is_cancelled());

        drop(second);
        assert!(cancel.is_cancelled());
    }

    #[kithara::test]
    fn initial_key_rules_keep_policy_order_and_global_header_semantics() {
        let ffi = crate::types::FfiKeyOptions {
            rules: vec![
                tagged_rule(1, "first-salt", &["keys.example.com"]),
                tagged_rule(2, "second-salt", &["*"]),
            ],
        };

        let (options, player_headers) = build_initial_key_state(ffi);

        assert_eq!(
            player_headers.get(SALT_HEADER).map(String::as_str),
            Some("second-salt"),
            "player-wide headers retain their existing last-rule-wins merge"
        );
        assert_eq!(
            player_headers.get("X-Provider").map(String::as_str),
            Some("2")
        );

        let registry = options.key_registry.expect("registry populated");
        let url = url::Url::parse("https://keys.example.com/key").expect("valid key URL");
        let request = registry.prepare(&url).expect("matching key request");

        assert_eq!(
            request.headers.get(SALT_HEADER).map(String::as_str),
            Some("first-salt"),
            "the first matching domain rule prepares the key request"
        );
        assert_eq!(
            request.headers.get("X-Provider").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            (request.processor)(Bytes::from_static(b"encrypted")).expect("processor succeeds"),
            Bytes::from_static(&[1])
        );
    }

    #[kithara::test]
    fn runtime_key_rules_append_in_registration_order() {
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        inner.setup_hls_aes_with_rule(tagged_rule(1, "first-salt", &["keys.example.com"]));
        inner.setup_hls_aes_with_rule(tagged_rule(2, "second-salt", &["*"]));

        let registry = inner
            .key_options
            .lock()
            .key_registry
            .clone()
            .expect("registry populated");
        let url = url::Url::parse("https://keys.example.com/key").expect("valid key URL");
        let request = registry.prepare(&url).expect("matching key request");

        assert_eq!(
            request.headers.get(SALT_HEADER).map(String::as_str),
            Some("first-salt")
        );
        assert_eq!(
            (request.processor)(Bytes::from_static(b"encrypted")).expect("processor succeeds"),
            Bytes::from_static(&[1])
        );
        assert_eq!(
            inner
                .player_headers
                .get(SALT_HEADER)
                .map(|header| header.value().clone())
                .as_deref(),
            Some("second-salt")
        );
    }

    #[kithara::test]
    fn setup_network_writes_auth_token_into_player_headers() {
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        inner.setup_network("token-123".to_string());
        let token = inner
            .player_headers
            .get(AUTH_TOKEN_HEADER)
            .map(|r| r.value().clone());
        assert_eq!(token.as_deref(), Some("token-123"));
    }

    #[kithara::test]
    fn setup_network_clears_auth_token_when_empty() {
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        inner.setup_network("token-123".to_string());
        inner.setup_network(String::new());
        assert!(!inner.player_headers.contains_key(AUTH_TOKEN_HEADER));
    }

    #[kithara::test]
    fn setup_hls_aes_registers_wildcard_rule_with_prod_salt() {
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        // Registration must not run the processor: an unstubbed `Unimock`
        // panics if `setup_hls_aes` calls it.
        inner.setup_hls_aes(Arc::new(Unimock::new(())));

        let salt = inner
            .player_headers
            .get(SALT_HEADER)
            .map(|r| r.value().clone())
            .expect("salt header populated");
        assert_eq!(salt.len(), 8, "prod auto-salt length");
        assert!(
            salt.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "prod auto-salt must be lowercase hex, got {salt:?}"
        );

        let key_options = inner.key_options.lock().clone();
        assert!(
            key_options.key_registry.is_some(),
            "registry must hold the wildcard rule"
        );
    }

    #[kithara::test]
    fn update_peak_bitrate_remembers_both_limits() {
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        inner.update_peak_bitrate(2_000_000.0, 500_000.0);
        let snapshot = *inner.peak_bitrate.lock();
        assert!((snapshot.wifi_bps - 2_000_000.0).abs() < f64::EPSILON);
        assert!((snapshot.cellular_bps - 500_000.0).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn peak_bitrate_effective_cap_picks_min_non_zero() {
        let pb = PeakBitrate {
            wifi_bps: 2_000_000.0,
            cellular_bps: 500_000.0,
        };
        assert_eq!(pb.effective_cap(), Some(500_000));

        let pb = PeakBitrate {
            wifi_bps: 0.0,
            cellular_bps: 750_000.0,
        };
        assert_eq!(pb.effective_cap(), Some(750_000));

        let pb = PeakBitrate {
            wifi_bps: 0.0,
            cellular_bps: 0.0,
        };
        assert_eq!(pb.effective_cap(), None);
    }
}
