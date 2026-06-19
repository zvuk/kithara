use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use kithara::{
    abr::AbrMode,
    audio::generate_log_spaced_bands,
    hls::{KeyOptions, KeyProcessorRegistry, KeyProcessorRule},
    net::{HttpClient, NetOptions},
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_drm::{KeyRequest, KeyRequestFactory};
use kithara_platform::{CancelToken, sync::Mutex};
use kithara_queue::{Queue, QueueConfig, QueueError, TrackSource, Transition};

use super::salt;
use crate::{
    config::StoreOptions,
    event_bridge::EventBridge,
    item::AudioPlayerItem,
    native_config,
    observer::{AUTH_TOKEN_HEADER, FfiKeyProcessor, PlayerObserver, SALT_HEADER, SeekCallback},
    registry::ItemRegistry,
    types::{
        FfiAbrMode, FfiError, FfiKeyRule, FfiPlayerConfig, FfiPlayerSnapshot, FfiPlayerStatus,
    },
};

fn build_processor_closure(
    processor: Arc<dyn FfiKeyProcessor>,
    salt: String,
) -> kithara_drm::KeyProcessor {
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

/// Build a core-level [`KeyProcessorRule`] from an FFI rule. Wraps
/// the rule's salt into a [`KeyRequestFactory`] so each fetch
/// produces the salt + processor pair atomically. If the FFI caller
/// pinned a salt via [`FfiKeyRule::salt`], we honour it (legacy
/// behaviour — same salt every time); otherwise we synthesise an
/// empty salt and leave per-request rotation to a higher layer.
fn build_processor_rule(rule: FfiKeyRule) -> KeyProcessorRule {
    let processor = rule.processor;
    let salt_template = rule.salt.unwrap_or_default();
    let factory: KeyRequestFactory = Arc::new(move || {
        let salt = salt_template.clone();
        let mut headers = HashMap::new();
        headers.insert(SALT_HEADER.to_string(), salt.clone());
        let proc = build_processor_closure(Arc::clone(&processor), salt);
        KeyRequest::new(headers, proc)
    });
    KeyProcessorRule::for_domains(&rule.domains, factory)
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
        return (KeyOptions::new(), HashMap::new());
    }
    let mut registry = KeyProcessorRegistry::new();
    let mut player_headers: HashMap<String, String> = HashMap::new();
    for r in ffi.rules {
        if let Some(headers) = r.headers.as_ref() {
            for (k, v) in headers {
                player_headers.insert(k.clone(), v.clone());
            }
        }
        if let Some(salt) = r.salt.as_ref() {
            player_headers.insert(SALT_HEADER.to_string(), salt.clone());
        }
        registry.add(build_processor_rule(r));
    }
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
    queue: Arc<Queue>,
    /// Master cancel token for this FFI player instance. Propagated
    /// into [`PlayerConfig::cancel`] so the audio worker, downloader,
    /// asset store, HLS peer, and per-track resources all derive
    /// children of this token. The facade's `Drop` fires
    /// `cancel.cancel()` so the shutdown pulse reaches subsystems before
    /// structural Arc teardown unwinds. See `kithara-play/CONTEXT.md`
    /// "Cancel Hierarchy". The chain flag reaches the audio worker and HLS
    /// coord lock-free `is_cancelled()` reads; every subsystem derives its
    /// own [`CancelToken::child`] from this consumer-top master.
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
    /// Shared storage options (cache dir, etc.) applied to every item.
    store: StoreOptions,
}

impl NativeInner {
    pub(crate) fn new(config: FfiPlayerConfig) -> Self {
        let cancel = CancelToken::root();
        let player_config = PlayerConfig::builder()
            .eq_layout(generate_log_spaced_bands(config.eq_band_count as usize))
            .cancel(cancel.child())
            .build();
        let player = Arc::new(PlayerImpl::new(player_config));
        let queue_config = QueueConfig::default().with_player(player);
        let net = default_net_options();
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(net, cancel.child()))
                .runtime(crate::FFI_RUNTIME.clone())
                .build(),
        );
        let (key_options, player_headers) = build_initial_key_state(config.key_options);
        let player_headers_map: DashMap<String, String> = player_headers.into_iter().collect();
        Self {
            downloader,
            shutdown: cancel,
            key_options: Mutex::new(key_options),
            player_headers: player_headers_map,
            peak_bitrate: Mutex::default(),
            queue: Arc::new(Queue::new(queue_config)),
            store: config.store,
            observer: Mutex::default(),
            event_bridge: Mutex::default(),
            items: Arc::new(Mutex::default()),
        }
    }

    pub(crate) fn advance_to_next_item(&self) {
        let _rt = crate::FFI_RUNTIME.enter();
        let tracks = self.queue.tracks();
        let Some(current_idx) = self.queue.current_index() else {
            return;
        };
        let next_idx = current_idx + 1;
        let Some(next) = tracks.get(next_idx) else {
            return;
        };
        let _ = self.queue.select(next.id, Transition::None);
    }

    pub(crate) fn append(&self, item: &Arc<AudioPlayerItem>) -> Result<(), FfiError> {
        let _rt = crate::FFI_RUNTIME.enter();
        let source = build_source_for_item(self, item)?;
        let id = item.track_id();
        self.queue.append_with_id(id, source);
        *item.inserted.lock() = true;
        self.items.lock().insert(id, Arc::clone(item));
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn crossfade_duration(&self) -> f32 {
        self.queue.crossfade_duration()
    }

    pub(crate) fn current_item(&self) -> Option<Arc<AudioPlayerItem>> {
        let entry = self.queue.current()?;
        self.items.lock().get(&entry.id).cloned()
    }

    pub(crate) fn current_time(&self) -> f64 {
        self.queue.position_seconds().unwrap_or(0.0)
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
        let _rt = crate::FFI_RUNTIME.enter();
        let source = build_source_for_item(self, item)?;
        let id = item.track_id();
        let after_id = after.map(|i| i.track_id());

        self.queue
            .insert_with_id(id, source, after_id)
            .map_err(|e| FfiError::InvalidArgument {
                reason: e.to_string(),
            })?;

        *item.inserted.lock() = true;
        self.items.lock().insert(id, Arc::clone(item));
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn is_muted(&self) -> bool {
        self.queue.is_muted()
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

    pub(crate) fn pause(&self) {
        self.queue.pause();
    }

    pub(crate) fn play(&self) {
        self.queue.play();
    }

    pub(crate) fn playing_rate(&self) -> f32 {
        self.queue.default_rate()
    }

    pub(crate) fn rate(&self) -> f32 {
        self.queue.rate()
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
        let _rt = crate::FFI_RUNTIME.enter();
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

    pub(crate) fn reset_eq(&self) -> Result<(), FfiError> {
        self.queue.reset_eq().map_err(FfiError::from)
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

    pub(crate) fn select_item(
        &self,
        index: u32,
        transition: crate::types::FfiTransition,
    ) -> Result<(), FfiError> {
        let _rt = crate::FFI_RUNTIME.enter();
        let tracks = self.queue.tracks();
        let idx = index as usize;
        let entry = tracks.get(idx).ok_or_else(|| FfiError::InvalidArgument {
            reason: format!("item index {idx} out of range (len: {})", tracks.len()),
        })?;
        self.queue
            .select(entry.id, transition.into())
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

    pub(crate) fn set_crossfade_duration(&self, seconds: f32) {
        self.queue.set_crossfade_duration(seconds);
    }

    pub(crate) fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), FfiError> {
        self.queue
            .set_eq_gain(band as usize, gain_db)
            .map_err(FfiError::from)
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        self.queue.set_muted(muted);
    }

    pub(crate) fn set_observer(&self, observer: Arc<dyn PlayerObserver>) {
        let rx = self.queue.subscribe();

        let bridge = EventBridge::spawn(
            rx,
            Arc::clone(&observer),
            Arc::clone(&self.queue),
            &self.items,
            CancelToken::never(),
        );

        let mut eb = self.event_bridge.lock();
        let mut obs = self.observer.lock();
        *eb = Some(bridge);
        *obs = Some(observer);
        drop(obs);
        drop(eb);
    }

    pub(crate) fn set_playing_rate(&self, rate: f32) {
        self.queue.set_default_rate(rate);
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        self.queue.set_volume(volume);
    }

    pub(crate) fn setup_hls_aes(&self, processor: Arc<dyn FfiKeyProcessor>) {
        let salt = salt::drm_prod_salt();
        let mut rule_headers = HashMap::new();
        rule_headers.insert(SALT_HEADER.to_string(), salt.clone());
        let rule = FfiKeyRule {
            processor,
            headers: Some(rule_headers),
            query_params: None,
            domains: vec!["*".to_string()],
            salt: Some(salt.clone()),
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
        registry.add(processor_rule);
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
        FfiPlayerSnapshot {
            status: FfiPlayerStatus::from(self.queue.status()),
            current_time: self.queue.position_seconds(),
            duration: self.queue.duration_seconds(),
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

    pub(crate) fn volume(&self) -> f32 {
        self.queue.volume()
    }
}

/// Build a [`TrackSource::Config`] from the item's fields. Also attaches
/// a scoped bus so the item's per-resource event bridge captures events
/// published during `Resource::new` (`VariantsDiscovered` fires
/// synchronously during stream open — a late subscriber would miss it).
fn build_source_for_item(
    inner: &NativeInner,
    item: &Arc<AudioPlayerItem>,
) -> Result<TrackSource, FfiError> {
    let scoped = inner.queue.bus().scoped();
    let abr_mode = item.abr_mode().map(|mode| match mode {
        FfiAbrMode::Auto => AbrMode::Auto(None),
        FfiAbrMode::Manual { variant_index } => AbrMode::manual(variant_index as usize),
    });
    let mut config = ResourceConfig::for_src(item.url())
        .map_err(|e| FfiError::InvalidArgument {
            reason: e.to_string(),
        })?
        .preferred_peak_bitrate(item.preferred_peak_bitrate().max(0.0))
        .maybe_headers(merged_headers_for_item(inner, item).map(Into::into))
        .events(scoped.clone())
        .downloader(inner.downloader.clone())
        .keys(inner.key_options.lock().clone())
        .initial_abr_mode(abr_mode.unwrap_or_default())
        .build();

    native_config::configure_resource(&mut config, &inner.store);
    *item.bus.lock() = Some(scoped);

    Ok(TrackSource::Config(Box::new(config)))
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
    /// Fire the master cancel pulse so the shutdown signal reaches
    /// subsystems before structural Arc teardown unwinds. The facade
    /// owns `NativeInner` by value, so this runs exactly when the
    /// `AudioPlayer` is dropped. See `kithara-play/CONTEXT.md`
    /// "Cancel Hierarchy".
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::FfiKeyProcessor;

    #[kithara::test]
    fn setup_network_writes_auth_token_into_player_headers() {
        let inner = NativeInner::new(FfiPlayerConfig::default());
        inner.setup_network("token-123".to_string());
        let token = inner
            .player_headers
            .get(AUTH_TOKEN_HEADER)
            .map(|r| r.value().clone());
        assert_eq!(token.as_deref(), Some("token-123"));
    }

    #[kithara::test]
    fn setup_network_clears_auth_token_when_empty() {
        let inner = NativeInner::new(FfiPlayerConfig::default());
        inner.setup_network("token-123".to_string());
        inner.setup_network(String::new());
        assert!(!inner.player_headers.contains_key(AUTH_TOKEN_HEADER));
    }

    #[kithara::test]
    fn setup_hls_aes_registers_wildcard_rule_with_prod_salt() {
        struct DummyProcessor;
        impl FfiKeyProcessor for DummyProcessor {
            fn process_key(&self, _key: Vec<u8>, _salt: String) -> Vec<u8> {
                Vec::new()
            }
        }

        let inner = NativeInner::new(FfiPlayerConfig::default());
        inner.setup_hls_aes(Arc::new(DummyProcessor));

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
        let inner = NativeInner::new(FfiPlayerConfig::default());
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
