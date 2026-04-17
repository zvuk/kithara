//! FFI wrapper for the audio player.
//!
//! `AudioPlayer` owns a [`kithara_queue::Queue`] under the hood. All queue
//! bookkeeping (stable `TrackId`s, auto-respawn of consumed resources,
//! crossfade-aware select, auto-advance on `ItemDidPlayToEnd`) lives in
//! the Queue. The FFI layer is a thin adapter: index-based Swift API
//! maps onto `TrackId`-based Queue calls through a `TrackId ->
//! AudioPlayerItem` map that preserves Swift-side identity.

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara::{
    abr::{AbrController, AbrMode, AbrOptions},
    audio::generate_log_spaced_bands,
    hls::{KeyOptions, KeyProcessorRegistry, KeyProcessorRule},
    net::NetOptions,
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_events::TrackId;
use kithara_platform::Mutex;
use kithara_queue::{Queue, QueueConfig, QueueError, TrackSource};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{self, StoreOptions},
    event_bridge::EventBridge,
    item::AudioPlayerItem,
    observer::{PlayerObserver, SeekCallback},
    types::{FfiAbrMode, FfiError, FfiPlayerConfig, FfiPlayerSnapshot, FfiPlayerStatus},
};

/// FFI-facing audio player.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct AudioPlayer {
    queue: Arc<Queue>,
    /// Shared downloader for every track created through this player.
    /// Pinned to `FFI_RUNTIME` so its async tasks land on a runtime that
    /// is always alive, independent of the caller thread (Swift /
    /// Kotlin callbacks run without an ambient tokio context).
    downloader: Downloader,
    /// Shared storage options (cache dir, etc.) applied to every item.
    store: StoreOptions,
    /// Immutable [`KeyOptions`] built once from [`FfiPlayerConfig`].
    key_options: KeyOptions,
    observer: Mutex<Option<Arc<dyn PlayerObserver>>>,
    event_bridge: Mutex<Option<EventBridge>>,
    /// Swift-owned items indexed by `TrackId`. Populated by [`insert`],
    /// drained by [`remove`] / [`remove_all_items`]. Lets [`items`] return
    /// the same `AudioPlayerItem` instances that Swift handed in (preserves
    /// identity + active per-item observer wiring).
    items: Arc<Mutex<HashMap<TrackId, Arc<AudioPlayerItem>>>>,
}

/// Convert the FFI-level [`crate::types::FfiKeyOptions`] into a
/// core-level [`KeyOptions`] with a populated registry.
fn build_key_options(ffi: crate::types::FfiKeyOptions) -> KeyOptions {
    if ffi.rules.is_empty() {
        return KeyOptions::new();
    }
    let mut registry = KeyProcessorRegistry::new();
    for r in ffi.rules {
        let processor = r.processor;
        let proc: kithara_drm::KeyProcessor =
            Arc::new(move |key: Bytes| Ok(Bytes::from(processor.process_key(key.to_vec()))));
        let mut rule = KeyProcessorRule::new(&r.domains, proc);
        if let Some(h) = r.headers {
            rule = rule.with_headers(h);
        }
        if let Some(q) = r.query_params {
            rule = rule.with_query_params(q);
        }
        registry.add(rule);
    }
    KeyOptions::new().with_key_registry(registry)
}

/// Methods exported across the FFI boundary.
#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
impl AudioPlayer {
    #[must_use]
    #[cfg_attr(feature = "backend-uniffi", uniffi::constructor)]
    pub fn new(config: FfiPlayerConfig) -> Arc<Self> {
        let player_config = PlayerConfig {
            eq_layout: generate_log_spaced_bands(config.eq_band_count as usize),
            ..PlayerConfig::default()
        };
        let player = Arc::new(PlayerImpl::new(player_config));
        let queue_config = QueueConfig::default()
            .with_player(player)
            .with_autoplay(false);
        #[cfg(feature = "dev")]
        let net = NetOptions {
            insecure: true,
            ..NetOptions::default()
        };
        #[cfg(not(feature = "dev"))]
        let net = NetOptions::default();
        let downloader = Downloader::new(
            DownloaderConfig::default()
                .with_net(net)
                .with_runtime(crate::FFI_RUNTIME.clone()),
        );
        let key_options = build_key_options(config.key_options);
        Arc::new(Self {
            queue: Arc::new(Queue::new(queue_config)),
            downloader,
            store: config.store,
            key_options,
            observer: Mutex::new(None),
            event_bridge: Mutex::new(None),
            items: Arc::new(Mutex::new(HashMap::new())),
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
    /// The callback is invoked synchronously with `true` if the seek
    /// command was accepted, `false` otherwise (matches `AVPlayer`
    /// semantics).
    #[expect(clippy::needless_pass_by_value, reason = "UniFFI requires owned Arc")]
    pub fn seek(&self, to_seconds: f64, callback: Arc<dyn SeekCallback>) {
        match self.queue.seek(to_seconds) {
            Ok(()) => callback.on_complete(true),
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
    #[expect(clippy::needless_pass_by_value, reason = "UniFFI requires owned Arc")]
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
                    reason: format!("item {} not found in queue", after_item.id()),
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
            reason: format!("item {} not in queue", item.id()),
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
        u32::try_from(self.queue.len()).unwrap_or(u32::MAX)
    }

    /// Replace the item at `index` with a freshly-configured one.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if `index` is out of range
    /// or the item's URL is malformed.
    #[expect(clippy::needless_pass_by_value, reason = "UniFFI requires owned Arc")]
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

    pub fn default_rate(&self) -> f32 {
        self.queue.default_rate()
    }

    pub fn set_default_rate(&self, rate: f32) {
        self.queue.set_default_rate(rate);
    }

    pub fn set_abr_mode(&self, mode: FfiAbrMode) {
        let abr_mode = match mode {
            FfiAbrMode::Auto => AbrMode::Auto(None),
            FfiAbrMode::Manual { variant_index } => AbrMode::Manual(variant_index as usize),
        };
        self.queue.player().set_abr_mode(abr_mode);
    }

    pub fn rate(&self) -> f32 {
        self.queue.player().rate()
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

    #[expect(clippy::cast_possible_truncation, reason = "EQ band count fits u32")]
    pub fn eq_band_count(&self) -> u32 {
        self.queue.eq_band_count() as u32
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
        let player = self.queue.player();
        FfiPlayerSnapshot {
            status: FfiPlayerStatus::from(player.status()),
            current_time: player.position_seconds(),
            duration: player.duration_seconds(),
            rate: player.rate(),
            default_rate: player.default_rate(),
            volume: player.volume(),
            muted: player.is_muted(),
        }
    }

    pub fn set_observer(self: &Arc<Self>, observer: Arc<dyn PlayerObserver>) {
        let rx = self.queue.subscribe();

        let bridge = EventBridge::spawn(
            rx,
            Arc::clone(&observer),
            Arc::clone(&self.queue),
            &self.items,
            CancellationToken::new(),
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
    #[expect(dead_code, reason = "reserved for future event bridge extensions")]
    pub(crate) fn observer(&self) -> Option<Arc<dyn PlayerObserver>> {
        self.observer.lock_sync().clone()
    }

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

        if let Some(headers) = item.headers() {
            config = config.with_headers(headers.into());
        }

        let scoped = self.queue.player().bus().scoped();
        config.bus = Some(scoped.clone());
        *item.bus.lock_sync() = Some(scoped);

        config = config.with_downloader(self.downloader.clone());

        if self.key_options.key_registry.is_some() {
            config = config.with_keys(self.key_options.clone());
        }

        if let Some(mode) = item.abr_mode() {
            let abr_mode = match mode {
                FfiAbrMode::Auto => AbrMode::Auto(None),
                FfiAbrMode::Manual { variant_index } => AbrMode::Manual(variant_index as usize),
            };
            config.abr = Some(AbrController::new(AbrOptions {
                mode: abr_mode,
                ..AbrOptions::default()
            }));
        }

        Ok(TrackSource::Config(Box::new(config)))
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
    fn default_rate_roundtrip() {
        let player = AudioPlayer::new(FfiPlayerConfig::default());
        assert!((player.default_rate() - 1.0).abs() < f32::EPSILON);
        player.set_default_rate(0.5);
        assert!((player.default_rate() - 0.5).abs() < f32::EPSILON);
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
}
