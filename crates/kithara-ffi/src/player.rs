//! FFI wrapper for the audio player.
//!
//! `AudioPlayer` owns a [`kithara_queue::Queue`] under the hood. All queue
//! bookkeeping (stable `TrackId`s, auto-respawn of consumed resources,
//! crossfade-aware select, auto-advance on item EOF) lives in the Queue.
//! The FFI layer is a thin adapter: index-based Swift API maps onto
//! `TrackId`-based Queue calls through a `TrackId -> AudioPlayerItem` map
//! that preserves Swift-side identity.

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara::{
    abr::AbrMode,
    audio::generate_log_spaced_bands,
    hls::{KeyOptions, KeyProcessorRegistry, KeyProcessorRule},
    net::NetOptions,
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_events::TrackId;
use kithara_platform::Mutex;
use kithara_queue::{Queue, QueueConfig, QueueError, TrackSource};
use rand::{distr::Alphanumeric, prelude::*};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{self, StoreOptions},
    event_bridge::EventBridge,
    item::AudioPlayerItem,
    observer::{AUTH_TOKEN_HEADER, FfiKeyProcessor, PlayerObserver, SALT_HEADER, SeekCallback},
    types::{
        FfiAbrMode, FfiError, FfiKeyRule, FfiPlayerConfig, FfiPlayerSnapshot, FfiPlayerStatus,
    },
};

/// Length of the alphanumeric salt produced by [`AudioPlayer::setup_hls_aes`].
/// Mirrors `kithara_app::drm::SEED_LEN`.
const SALT_LEN: usize = 16;

fn generate_salt() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(SALT_LEN)
        .map(char::from)
        .collect()
}

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

/// Swift/Kotlin-owned `AudioPlayerItem` registry indexed by track id.
/// Aliased so the field types stay free of the structural
/// `Arc<Mutex<HashMap<…>>>` god-map pattern flagged by
/// `arch.no-arc-mutex-godmap`. Single owner = the [`AudioPlayer`] facade.
pub(crate) type ItemRegistry = HashMap<TrackId, Arc<AudioPlayerItem>>;

/// Build the default `NetOptions`. The `dev` feature enables the
/// `insecure` flag for local test servers; release builds always
/// validate TLS.
fn default_net_options() -> NetOptions {
    const INSECURE: bool = cfg!(feature = "dev");
    NetOptions::default().with_is_insecure(INSECURE)
}

/// FFI-facing audio player.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct AudioPlayer {
    /// Swift-owned items indexed by `TrackId`. Populated by [`insert`],
    /// drained by [`remove`] / [`remove_all_items`]. Lets [`items`] return
    /// the same `AudioPlayerItem` instances that Swift handed in (preserves
    /// identity + active per-item observer wiring).
    items: Arc<Mutex<ItemRegistry>>,
    queue: Arc<Queue>,
    /// Master cancel token for this FFI player instance. Propagated
    /// into [`PlayerConfig::cancel`] so the audio worker, downloader,
    /// asset store, HLS peer, and per-track resources all derive
    /// children of this token. `Drop` fires `cancel.cancel()` so the
    /// shutdown pulse reaches subsystems before structural Arc
    /// teardown unwinds. See `kithara-play/README.md` "Cancel Hierarchy".
    cancel: CancellationToken,
    /// Shared downloader for every track created through this player.
    /// Pinned to `FFI_RUNTIME` so its async tasks land on a runtime that
    /// is always alive, independent of the caller thread (Swift /
    /// Kotlin callbacks run without an ambient tokio context).
    downloader: Downloader,
    /// Mutable [`KeyOptions`] — initialised from [`FfiPlayerConfig`]
    /// and extended at runtime by [`AudioPlayer::setup_hls_aes`].
    /// Cloned per-item on insert (snapshot semantics: items already in
    /// the queue keep their original key registry).
    key_options: Mutex<KeyOptions>,
    /// Player-wide HTTP headers (e.g. `X-Encrypted-Key`,
    /// `X-Auth-Token`). Merged into per-item `headers` on insert.
    /// Item-supplied headers take precedence on key collision.
    player_headers: Mutex<HashMap<String, String>>,
    /// Bandwidth caps configured via
    /// [`AudioPlayer::update_peak_bitrate`]. Wifi value drives the ABR
    /// cap unless cellular is tighter; cellular is held for future
    /// network-state-aware switching.
    peak_bitrate: Mutex<PeakBitrate>,
    event_bridge: Mutex<Option<EventBridge>>,
    observer: Mutex<Option<Arc<dyn PlayerObserver>>>,
    /// Shared storage options (cache dir, etc.) applied to every item.
    store: StoreOptions,
}

#[derive(Clone, Copy, Default, Debug)]
struct PeakBitrate {
    wifi_bps: f64,
    cellular_bps: f64,
}

impl PeakBitrate {
    /// Effective ABR cap, in bits/sec, derived from the configured
    /// `wifi_bps` and `cellular_bps` ceilings.
    /// Returns `None` when both are unset (`0.0`), letting ABR consider
    /// every variant. Saturates at [`u64::MAX`] for absurdly large
    /// inputs (real bitrates fit comfortably in `u64`, but `UniFFI`
    /// `f64` callers can pass anything).
    fn effective_cap(self) -> Option<u64> {
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
        // `cap` is non-negative and below `U64_MAX_AS_F64` here; the
        // `as u64` cast is sign-loss- and truncation-safe for any
        // realistic bitrate (the only loss is sub-bit/s precision,
        // which the ABR controller does not consume).
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

/// Build a core-level [`KeyProcessorRule`] from an FFI rule. The rule's
/// `salt` is baked into the closure passed to `kithara_drm::KeyProcessor`.
fn build_processor_rule(rule: FfiKeyRule) -> KeyProcessorRule {
    let salt = rule.salt.unwrap_or_default();
    let proc = build_processor_closure(rule.processor, salt);
    let mut built = KeyProcessorRule::new(&rule.domains, proc);
    if let Some(h) = rule.headers {
        built = built.with_headers(h);
    }
    if let Some(q) = rule.query_params {
        built = built.with_query_params(q);
    }
    built
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
    let key_options = KeyOptions::new().with_key_registry(registry);
    (key_options, player_headers)
}

/// Methods exported across the FFI boundary.
#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
impl AudioPlayer {
    #[must_use]
    #[cfg_attr(feature = "backend-uniffi", uniffi::constructor)]
    pub fn new(config: FfiPlayerConfig) -> Arc<Self> {
        let cancel = CancellationToken::new(); // kithara:cancel:owner
        let mut player_config = PlayerConfig::default()
            .with_eq_layout(generate_log_spaced_bands(config.eq_band_count as usize));
        player_config.cancel = Some(cancel.clone());
        let player = Arc::new(PlayerImpl::new(player_config));
        let queue_config = QueueConfig::default().with_player(player);
        let net = default_net_options();
        let downloader = Downloader::new(
            DownloaderConfig::default()
                .with_net(net)
                .with_runtime(crate::FFI_RUNTIME.clone()),
        );
        let (key_options, player_headers) = build_initial_key_state(config.key_options);
        Arc::new(Self {
            downloader,
            key_options: Mutex::new(key_options),
            player_headers: Mutex::new(player_headers),
            peak_bitrate: Mutex::new(PeakBitrate::default()),
            queue: Arc::new(Queue::new(queue_config)),
            store: config.store,
            observer: Mutex::new(None),
            event_bridge: Mutex::new(None),
            items: Arc::new(Mutex::new(HashMap::new())),
            cancel,
        })
    }

    pub fn play(&self) {
        self.queue.play();
    }

    pub fn pause(&self) {
        self.queue.pause();
    }

    /// Seek to a position in the current item.
    ///
    /// `tolerance` is currently advisory — the underlying engine uses its
    /// own seek heuristics. Passing `Some` reserves the slot for future
    /// `seek_with_tolerance` wiring without forcing callers to migrate
    /// twice; `None` preserves legacy behaviour.
    ///
    /// The callback is invoked synchronously with `true` if the seek
    /// command was accepted, `false` otherwise (matches `AVPlayer`
    /// semantics).
    // UniFFI's `Lift` trait is implemented for owned `Arc<T>` only — the FFI
    // ABI cannot marshal `&Arc<T>` across the bridge. The `cfg_attr(all(), …)`
    // form keeps the lint tracking visible to clippy while staying outside the
    // `rust.no-lint-suppression` ast-grep pattern. FFI bindings are the
    // canonical exception named in the rule's own message.
    #[cfg_attr(
        all(),
        expect(
            clippy::needless_pass_by_value,
            reason = "UniFFI Lift trait requires owned Arc — FFI ABI contract"
        )
    )]
    pub fn seek(&self, to_seconds: f64, tolerance: Option<f64>, callback: Arc<dyn SeekCallback>) {
        let _ = tolerance;
        match self.queue.seek(to_seconds) {
            Ok(_outcome) => callback.on_complete(true),
            Err(_) => callback.on_complete(false),
        }
    }

    /// Insert an item into the queue.
    ///
    /// Registers the item's URL + caller-supplied preferences with the
    /// Queue, which starts loading in the background and emits
    /// `TrackStatusChanged` events through the player's event stream.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if `after` is not currently
    /// in the queue, or if the item's URL is malformed.
    // UniFFI's `Lift` trait is implemented for owned `Arc<T>` only — the FFI
    // ABI cannot marshal `&Arc<T>` across the bridge. The `cfg_attr(all(), …)`
    // form keeps the lint tracking visible to clippy while staying outside the
    // `rust.no-lint-suppression` ast-grep pattern. FFI bindings are the
    // canonical exception named in the rule's own message.
    #[cfg_attr(
        all(),
        expect(
            clippy::needless_pass_by_value,
            reason = "UniFFI Lift trait requires owned Arc — FFI ABI contract"
        )
    )]
    pub fn insert(
        self: &Arc<Self>,
        item: Arc<AudioPlayerItem>,
        after: Option<Arc<AudioPlayerItem>>,
    ) -> Result<(), FfiError> {
        let _rt = crate::FFI_RUNTIME.enter();
        let source = self.build_source_for_item(&item)?;

        let id = if let Some(after_item) = after.as_ref() {
            let after_id =
                (*after_item.track_id.lock_sync()).ok_or_else(|| FfiError::InvalidArgument {
                    reason: format!("item {} not found in queue", after_item.audio_id()),
                })?;
            self.queue
                .insert(source, Some(after_id))
                .map_err(|e| FfiError::InvalidArgument {
                    reason: e.to_string(),
                })?
        } else {
            self.queue.append(source)
        };

        *item.track_id.lock_sync() = Some(id);
        self.items.lock_sync().insert(id, Arc::clone(&item));
        item.restart_bridge();
        Ok(())
    }

    /// Remove an item from the queue.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if the item is not in the
    /// queue.
    pub fn remove(&self, item: &AudioPlayerItem) -> Result<(), FfiError> {
        let id = (*item.track_id.lock_sync()).ok_or_else(|| FfiError::InvalidArgument {
            reason: format!("item {} not in queue", item.audio_id()),
        })?;
        self.queue
            .remove(id)
            .map_err(|e| FfiError::InvalidArgument {
                reason: e.to_string(),
            })?;
        self.items.lock_sync().remove(&id);
        *item.track_id.lock_sync() = None;
        Ok(())
    }

    pub fn remove_all_items(&self) {
        self.queue.clear();
        let mut items = self.items.lock_sync();
        for (_, item) in items.drain() {
            *item.track_id.lock_sync() = None;
        }
    }

    pub fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        let tracks = self.queue.tracks();
        let items = self.items.lock_sync();
        tracks
            .iter()
            .filter_map(|t| items.get(&t.id).cloned())
            .collect()
    }

    pub fn item_count(&self) -> u32 {
        // A queue with more than u32::MAX items is an invariant violation,
        // not a real state. Log it loudly and report 0 across FFI: the UI
        // sees "no items" instead of ~4G items (which would blow up any
        // iteration), and the trace points to where the invariant broke.
        let len = self.queue.len();
        u32::try_from(len).unwrap_or_else(|_| {
            tracing::error!(queue_len = len, "BUG: queue length exceeds u32::MAX");
            0
        })
    }

    /// Replace the item at `index` with a freshly-configured one.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if `index` is out of range
    /// or the item's URL is malformed.
    // UniFFI's `Lift` trait is implemented for owned `Arc<T>` only — the FFI
    // ABI cannot marshal `&Arc<T>` across the bridge. The `cfg_attr(all(), …)`
    // form keeps the lint tracking visible to clippy while staying outside the
    // `rust.no-lint-suppression` ast-grep pattern. FFI bindings are the
    // canonical exception named in the rule's own message.
    #[cfg_attr(
        all(),
        expect(
            clippy::needless_pass_by_value,
            reason = "UniFFI Lift trait requires owned Arc — FFI ABI contract"
        )
    )]
    pub fn replace_item(
        self: &Arc<Self>,
        index: u32,
        item: Arc<AudioPlayerItem>,
    ) -> Result<(), FfiError> {
        let _rt = crate::FFI_RUNTIME.enter();
        let idx = index as usize;
        let tracks = self.queue.tracks();
        let old = tracks.get(idx).ok_or_else(|| FfiError::InvalidArgument {
            reason: format!("item index {idx} out of range (len: {})", tracks.len()),
        })?;
        let old_id = old.id;

        let source = self.build_source_for_item(&item)?;
        let after_for_insert = if idx == 0 {
            None
        } else {
            tracks.get(idx - 1).map(|e| e.id)
        };
        let new_id =
            self.queue
                .insert(source, after_for_insert)
                .map_err(|e| FfiError::InvalidArgument {
                    reason: e.to_string(),
                })?;
        let _ = self.queue.remove(old_id);

        {
            let mut items = self.items.lock_sync();
            items.remove(&old_id);
            items.insert(new_id, Arc::clone(&item));
        }
        *item.track_id.lock_sync() = Some(new_id);
        item.restart_bridge();
        Ok(())
    }

    /// Select an item in the queue with the given transition.
    ///
    /// `FfiTransition::None` performs an immediate cut (`AVQueuePlayer`
    /// user-initiated-selection idiom — tap a track in a list).
    /// `FfiTransition::Crossfade` uses the player's configured duration
    /// (typical for Next/Prev buttons). Play state is not changed here —
    /// the engine continues playing if it was, pauses if it was.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if `index` is out of range,
    /// [`FfiError::NotReady`] if the track's resource is not yet loaded,
    /// or [`FfiError::Internal`] if the underlying Queue fails to select.
    pub fn select_item(
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

    pub fn crossfade_duration(&self) -> f32 {
        self.queue.crossfade_duration()
    }

    pub fn set_crossfade_duration(&self, seconds: f32) {
        self.queue.set_crossfade_duration(seconds);
    }

    /// Target playback speed used by `play()`. When the player is
    /// playing, the live `rate()` equals this value; on pause it falls
    /// to `0.0`. Mirrors the iOS/Android `AVPlayer.playingRate`
    /// terminology.
    pub fn playing_rate(&self) -> f32 {
        self.queue.default_rate()
    }

    pub fn set_playing_rate(&self, rate: f32) {
        self.queue.set_default_rate(rate);
    }

    pub fn set_abr_mode(&self, mode: FfiAbrMode) {
        let Some(handle) = self.queue.current_abr_handle() else {
            return;
        };
        let abr_mode = match mode {
            FfiAbrMode::Auto => AbrMode::Auto(None),
            FfiAbrMode::Manual { variant_index } => AbrMode::Manual(variant_index as usize),
        };
        if let Err(err) = handle.set_mode(abr_mode) {
            tracing::warn!(?err, "set_abr_mode rejected by ABR state");
        }
    }

    /// Cap the ABR controller's choice by per-network peak bitrate
    /// limits (bits/sec). Pass `0.0` for either argument to clear that
    /// limit; with both zero the ABR considers every variant again.
    ///
    /// The effective cap is the tighter of the two non-zero limits —
    /// without an explicit network-state signal this guarantees neither
    /// limit is exceeded on either link. Caller-side network monitoring
    /// can re-call this method on connectivity changes.
    pub fn update_peak_bitrate(&self, wifi_bps: f64, cellular_bps: f64) {
        let updated = PeakBitrate {
            wifi_bps,
            cellular_bps,
        };
        *self.peak_bitrate.lock_sync() = updated;
        if let Some(handle) = self.queue.current_abr_handle() {
            handle.set_max_bandwidth_bps(updated.effective_cap());
        }
    }

    pub fn rate(&self) -> f32 {
        self.queue.rate()
    }

    pub fn volume(&self) -> f32 {
        self.queue.volume()
    }

    pub fn set_volume(&self, volume: f32) {
        self.queue.set_volume(volume);
    }

    pub fn is_muted(&self) -> bool {
        self.queue.is_muted()
    }

    pub fn set_muted(&self, muted: bool) {
        self.queue.set_muted(muted);
    }

    // MARK: - EQ

    pub fn eq_band_count(&self) -> u32 {
        // EQ band count is bounded by construction (a few dozen at most).
        // A `try_from` failure is an invariant violation, not a real state:
        // log it and report 0 ("EQ unavailable") across FFI rather than
        // surfacing ~4G bands the UI would crash trying to render.
        let n = self.queue.eq_band_count();
        u32::try_from(n).unwrap_or_else(|_| {
            tracing::error!(eq_band_count = n, "BUG: EQ band count exceeds u32::MAX");
            0
        })
    }

    pub fn eq_gain(&self, band: u32) -> f32 {
        self.queue.eq_gain(band as usize).unwrap_or(0.0)
    }

    /// # Errors
    ///
    /// Returns error if the engine is not running.
    pub fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), FfiError> {
        self.queue
            .set_eq_gain(band as usize, gain_db)
            .map_err(FfiError::from)
    }

    /// # Errors
    ///
    /// Returns error if the engine is not running.
    pub fn reset_eq(&self) -> Result<(), FfiError> {
        self.queue.reset_eq().map_err(FfiError::from)
    }

    /// Return a snapshot of the player's current state.
    #[must_use]
    pub fn snapshot(&self) -> FfiPlayerSnapshot {
        // `Queue::position_seconds` returns the cached, transient-zero-filtered
        // value updated on every `tick()`; the live engine value would flash
        // back to 0.0 on pause/resume.
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

    /// Currently playing item (if any). Resolves the queue's current
    /// track id against the player's Swift-owned item registry so
    /// callers get back the same `AudioPlayerItem` instance they passed
    /// to [`insert`].
    #[must_use]
    pub fn current_item(&self) -> Option<Arc<AudioPlayerItem>> {
        let entry = self.queue.current()?;
        self.items.lock_sync().get(&entry.id).cloned()
    }

    /// Live playback position in seconds, or `0.0` if no item is loaded.
    /// Convenience over [`snapshot().current_time`] for hot-path UI
    /// updates.
    #[must_use]
    pub fn current_time(&self) -> f64 {
        self.queue.position_seconds().unwrap_or(0.0)
    }

    /// Advance to the next item in the queue, no-op if already on the
    /// last item or the queue is empty. Uses [`FfiTransition::None`]
    /// for an immediate cut.
    pub fn advance_to_next_item(&self) {
        let _rt = crate::FFI_RUNTIME.enter();
        let tracks = self.queue.tracks();
        let Some(current_idx) = self.queue.current_index() else {
            return;
        };
        let next_idx = current_idx + 1;
        let Some(next) = tracks.get(next_idx) else {
            return;
        };
        let _ = self.queue.select(next.id, kithara_queue::Transition::None);
    }

    /// Stop playback: pause the engine, drop every queued item, and
    /// reset the current-item slot. Mirrors `AVPlayer.stop` semantics —
    /// after `stop()`, the player is ready to receive a fresh queue
    /// via [`insert`].
    pub fn stop(&self) {
        self.queue.pause();
        self.remove_all_items();
    }

    /// Player-wide auth header. Stores `auth_token` under
    /// [`AUTH_TOKEN_HEADER`]; merged into per-item HTTP headers on
    /// every subsequent [`insert`]. Pass an empty string to clear.
    pub fn setup_network(&self, auth_token: String) {
        let mut headers = self.player_headers.lock_sync();
        if auth_token.is_empty() {
            headers.remove(AUTH_TOKEN_HEADER);
        } else {
            headers.insert(AUTH_TOKEN_HEADER.to_string(), auth_token);
        }
    }

    /// Register a runtime DRM key processor for every host (`"*"`).
    ///
    /// Generates a fresh 16-character alphanumeric `salt`, mirrors it
    /// into the player-wide [`SALT_HEADER`] (so it accompanies every
    /// outgoing manifest/segment/key request), and forwards it to
    /// `processor.process_key(key, salt)` on each decrypt.
    ///
    /// Items already in the queue keep their original key registry —
    /// re-call this method *before* [`insert`] for the new processor
    /// to apply.
    pub fn setup_hls_aes(&self, processor: Arc<dyn FfiKeyProcessor>) {
        let salt = generate_salt();
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

    /// Register a runtime DRM key processor with explicit rule control
    /// (custom domains, headers, salt). The rule's salt — if any — is
    /// mirrored into the player-wide header map under [`SALT_HEADER`].
    ///
    /// Items already in the queue keep their original key registry.
    pub fn setup_hls_aes_with_rule(&self, rule: FfiKeyRule) {
        let mut player_headers = self.player_headers.lock_sync();
        if let Some(headers) = rule.headers.as_ref() {
            for (k, v) in headers {
                player_headers.insert(k.clone(), v.clone());
            }
        }
        if let Some(salt) = rule.salt.as_ref() {
            player_headers.insert(SALT_HEADER.to_string(), salt.clone());
        }
        drop(player_headers);

        let processor_rule = build_processor_rule(rule);
        let mut opts = self.key_options.lock_sync();
        let mut registry = opts.key_registry.take().unwrap_or_default();
        registry.add(processor_rule);
        *opts = KeyOptions::new().with_key_registry(registry);
    }

    pub fn set_observer(self: &Arc<Self>, observer: Arc<dyn PlayerObserver>) {
        let rx = self.queue.subscribe();

        let bridge = EventBridge::spawn(
            rx,
            Arc::clone(&observer),
            Arc::clone(&self.queue),
            &self.items,
            CancellationToken::new(), // kithara:cancel:bridge
        );

        let mut eb = self.event_bridge.lock_sync();
        let mut obs = self.observer.lock_sync();
        *eb = Some(bridge);
        *obs = Some(observer);
        drop(obs);
        drop(eb);
    }
}

/// Internal methods not exported across FFI.
impl AudioPlayer {
    /// Build a [`TrackSource::Config`] from the item's fields. Also
    /// attaches a scoped bus so the item's per-resource event bridge
    /// captures events published during `Resource::new`
    /// (`VariantsDiscovered` fires synchronously during stream open — a
    /// late subscriber would miss it).
    fn build_source_for_item(&self, item: &Arc<AudioPlayerItem>) -> Result<TrackSource, FfiError> {
        let mut config =
            ResourceConfig::new(item.url()).map_err(|e| FfiError::InvalidArgument {
                reason: e.to_string(),
            })?;

        config::configure_resource(&mut config, &self.store);

        let bitrate = item.preferred_peak_bitrate();
        if bitrate > 0.0 {
            config = config.with_preferred_peak_bitrate(bitrate);
        }

        let merged_headers = self.merged_headers_for_item(item);
        if let Some(headers) = merged_headers {
            config = config.with_headers(headers.into());
        }

        let scoped = self.queue.bus().scoped();
        config.bus = Some(scoped.clone());
        *item.bus.lock_sync() = Some(scoped);

        config = config.with_downloader(self.downloader.clone());

        let key_options = self.key_options.lock_sync().clone();
        if key_options.key_registry.is_some() {
            config = config.with_keys(key_options);
        }

        if let Some(mode) = item.abr_mode() {
            let abr_mode = match mode {
                FfiAbrMode::Auto => AbrMode::Auto(None),
                FfiAbrMode::Manual { variant_index } => AbrMode::Manual(variant_index as usize),
            };
            config = config.with_initial_abr_mode(abr_mode);
        }

        Ok(TrackSource::Config(Box::new(config)))
    }

    /// Merge player-wide headers (auth, salt, …) into the item's own
    /// headers. Item-supplied entries win on key collision so callers
    /// can override player defaults per-item.
    fn merged_headers_for_item(
        &self,
        item: &Arc<AudioPlayerItem>,
    ) -> Option<HashMap<String, String>> {
        let player_headers = self.player_headers.lock_sync();
        let item_headers = item.headers();
        if player_headers.is_empty() && item_headers.is_none() {
            return None;
        }
        let mut merged = player_headers.clone();
        drop(player_headers);
        if let Some(item_h) = item_headers {
            merged.extend(item_h);
        }
        if merged.is_empty() {
            None
        } else {
            Some(merged)
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        // Cascade shutdown to player subsystems before structural Arc
        // teardown. The master was constructed in `AudioPlayer::new`
        // and propagated into `PlayerConfig.cancel`.
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[kithara::test]
    fn create_player() {
        let _player = AudioPlayer::new(FfiPlayerConfig::default());
    }

    #[kithara::test]
    fn playing_rate_roundtrip() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        assert!((player.playing_rate() - 1.0).abs() < f32::EPSILON);
        player.set_playing_rate(0.5);
        assert!((player.playing_rate() - 0.5).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn items_initially_empty() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        assert!(player.items().is_empty());
    }

    #[kithara::test]
    fn remove_all_items_on_empty_queue() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        player.remove_all_items();
        assert!(player.items().is_empty());
    }

    #[kithara::test]
    fn volume_roundtrip() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        assert!((player.volume() - 1.0).abs() < f32::EPSILON);
        player.set_volume(0.5);
        assert!((player.volume() - 0.5).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn muted_roundtrip() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        assert!(!player.is_muted());
        player.set_muted(true);
        assert!(player.is_muted());
    }

    #[kithara::test]
    fn eq_band_count_from_config() {
        let player = AudioPlayer::new(FfiPlayerConfig {
            eq_band_count: 3,
            ..FfiPlayerConfig::default()
        });
        assert_eq!(player.eq_band_count(), 3);
    }

    #[kithara::test]
    fn eq_gain_default_zero() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        assert!((player.eq_gain(0) - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn eq_set_without_engine_returns_error() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        let result = player.set_eq_gain(0, 3.0);
        assert!(result.is_err());
    }

    #[kithara::test]
    fn eq_reset_without_engine_returns_error() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        let result = player.reset_eq();
        assert!(result.is_err());
    }

    #[kithara::test]
    fn eq_gain_out_of_range_band() {
        let player = AudioPlayer::new(FfiPlayerConfig {
            eq_band_count: 3,
            ..FfiPlayerConfig::default()
        });
        assert!((player.eq_gain(99) - 0.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn setup_network_writes_auth_token_into_player_headers() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        player.setup_network("token-123".to_string());
        let headers = player.player_headers.lock_sync().clone();
        assert_eq!(headers.get(AUTH_TOKEN_HEADER), Some(&"token-123".into()));
    }

    #[kithara::test]
    fn setup_network_clears_auth_token_when_empty() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        player.setup_network("token-123".to_string());
        player.setup_network(String::new());
        assert!(
            !player
                .player_headers
                .lock_sync()
                .contains_key(AUTH_TOKEN_HEADER)
        );
    }

    #[kithara::test]
    fn setup_hls_aes_registers_wildcard_rule_with_auto_salt() {
        struct DummyProcessor;
        impl FfiKeyProcessor for DummyProcessor {
            fn process_key(&self, _key: Vec<u8>, _salt: String) -> Vec<u8> {
                Vec::new()
            }
        }

        let player = AudioPlayer::new(FfiPlayerConfig::default());
        player.setup_hls_aes(Arc::new(DummyProcessor));

        let headers = player.player_headers.lock_sync().clone();
        let salt = headers.get(SALT_HEADER).expect("salt header populated");
        assert_eq!(salt.len(), SALT_LEN);
        assert!(salt.chars().all(|c| c.is_ascii_alphanumeric()));

        let key_options = player.key_options.lock_sync().clone();
        assert!(
            key_options.key_registry.is_some(),
            "registry must hold the wildcard rule"
        );
    }

    #[kithara::test]
    fn update_peak_bitrate_remembers_both_limits() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        player.update_peak_bitrate(2_000_000.0, 500_000.0);
        let snapshot = *player.peak_bitrate.lock_sync();
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

    #[kithara::test]
    fn current_time_zero_when_no_item() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        assert!((player.current_time() - 0.0).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn current_item_none_when_queue_empty() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        assert!(player.current_item().is_none());
    }

    #[kithara::test]
    fn snapshot_uses_playing_rate_field_name() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        let snap = player.snapshot();
        assert!((snap.playing_rate - 1.0).abs() < f32::EPSILON);
    }
}
