use kithara::platform::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use crate::config::FfiPlayerConfig;
use crate::{
    Inner,
    item::AudioPlayerItem,
    observer::{FfiKeyProcessor, PlayerObserver, SeekCallback},
    types::{
        FfiAbrMode, FfiActionAtItemEnd, FfiCrossfadeSettings, FfiError, FfiKeyRule,
        FfiPlaybackOrder, FfiPlayerSnapshot, FfiRepeatMode,
    },
};

/// FFI-facing audio player. A thin facade over the platform-selected
/// `Inner` engine (`NativeInner` on Apple / Android, `WasmInner` on
/// wasm32). Every exported method delegates straight to `inner`; the
/// facade only owns the object identity and (on native) the `Drop`
/// shutdown pulse. The JS control surface lives in
/// `crate::web::surface`.
#[cfg_attr(
    all(feature = "uniffi", not(target_arch = "wasm32")),
    derive(uniffi::Object)
)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub struct AudioPlayer {
    pub(crate) inner: Inner,
}

/// Methods exported across the FFI boundary.
#[cfg_attr(all(feature = "uniffi", not(target_arch = "wasm32")), uniffi::export)]
impl AudioPlayer {
    #[cfg(not(target_arch = "wasm32"))]
    #[cfg_attr(
        all(feature = "uniffi", not(target_arch = "wasm32")),
        uniffi::constructor
    )]
    /// # Errors
    /// Returns an error when the player configuration cannot be created,
    /// including a playing rate that is not a finite number.
    pub fn new(config: FfiPlayerConfig) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            inner: Inner::new(config)?,
        }))
    }

    pub fn action_at_item_end(&self) -> FfiActionAtItemEnd {
        self.inner.action_at_item_end()
    }

    /// Append an item to the tail of the queue. AVQueuePlayer-style
    /// counterpart of [`Self::insert`], which follows the iOS protocol
    /// shape (`after == nil` ⇒ head).
    ///
    /// # Errors
    ///
    /// Returns [`FfiError`] when the source URL cannot be resolved into
    /// a queue-owned `kithara::play::Source` — same failure surface as
    /// [`Self::insert`].
    #[cfg_attr(
        all(),
        expect(
            clippy::needless_pass_by_value,
            reason = "UniFFI Lift trait requires owned Arc — FFI ABI contract"
        )
    )]
    pub fn append(self: &Arc<Self>, item: Arc<AudioPlayerItem>) -> Result<(), FfiError> {
        self.inner.append(&item)
    }
    pub fn crossfade_settings(&self) -> FfiCrossfadeSettings {
        self.inner.crossfade_settings()
    }
    /// Currently playing item (if any). Resolves the queue's current
    /// track id against the player's Swift-owned item registry so
    /// callers get back the same `AudioPlayerItem` instance they passed
    /// to [`Self::insert`].
    #[must_use]
    pub fn current_item(&self) -> Option<Arc<AudioPlayerItem>> {
        self.inner.current_item()
    }

    /// Live playback position in seconds, or `0.0` if no item is loaded.
    /// Convenience over [`Self::snapshot`] and
    /// [`FfiPlayerSnapshot::current_time`] for hot-path UI updates.
    #[must_use]
    pub fn current_time(&self) -> f64 {
        self.inner.current_time()
    }

    pub fn eq_band_count(&self) -> u32 {
        self.inner.eq_band_count()
    }

    pub fn eq_gain(&self, band: u32) -> f32 {
        self.inner.eq_gain(band)
    }

    /// Inserts a URL and caller preferences, starts background queue loading, and
    /// emits `TrackStatusChanged` through the player event stream.
    /// `after == None` inserts at position 0, matching iOS
    /// `AudioPlayerProtocol.insert(_:after:)`; [`Self::append`] appends instead.
    ///
    /// # Errors
    /// Returns [`FfiError::InvalidArgument`] for an absent `after` item or malformed URL.
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
        self.inner.insert(&item, after.as_ref())
    }

    pub fn is_muted(&self) -> bool {
        self.inner.is_muted()
    }

    pub fn item_count(&self) -> u32 {
        self.inner.item_count()
    }

    pub fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        self.inner.items()
    }

    pub fn pause(&self) {
        self.inner.pause();
    }

    pub fn play(&self) {
        self.inner.play();
    }

    pub fn playback_order(&self) -> FfiPlaybackOrder {
        self.inner.playback_order()
    }

    /// Target playback speed used by `play()`. When the player is
    /// playing, the live `rate()` equals this value; on pause it falls
    /// to `0.0`. Mirrors the iOS/Android `AVPlayer.playingRate`
    /// terminology.
    pub fn playing_rate(&self) -> f32 {
        self.inner.playing_rate()
    }

    pub fn rate(&self) -> f32 {
        self.inner.rate()
    }

    /// Remove an item from the queue.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if the item is not in the
    /// queue.
    pub fn remove(&self, item: &AudioPlayerItem) -> Result<(), FfiError> {
        self.inner.remove(item)
    }

    pub fn remove_all_items(&self) {
        self.inner.remove_all_items();
    }

    /// Current queue repeat mode.
    #[must_use]
    pub fn repeat_mode(&self) -> FfiRepeatMode {
        self.inner.repeat_mode()
    }

    /// Replace the item at `index` with a freshly-configured one.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if `index` is out of range
    /// or the item's URL is malformed.
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
        self.inner.replace_item(index, &item)
    }

    /// # Errors
    ///
    /// Returns error if the engine is not running.
    pub fn reset_eq(&self) -> Result<(), FfiError> {
        self.inner.reset_eq()
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
    #[cfg_attr(
        all(),
        expect(
            clippy::needless_pass_by_value,
            reason = "UniFFI Lift trait requires owned Arc — FFI ABI contract"
        )
    )]
    pub fn seek(&self, to_seconds: f64, tolerance: Option<f64>, callback: Arc<dyn SeekCallback>) {
        self.inner.seek(to_seconds, tolerance, &callback);
    }

    pub fn set_abr_mode(&self, mode: FfiAbrMode) {
        self.inner.set_abr_mode(mode);
    }

    /// # Errors
    ///
    /// Returns error if the engine is not running.
    pub fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), FfiError> {
        self.inner.set_eq_gain(band, gain_db)
    }

    /// # Errors
    ///
    /// Returns the player's refusal of the change; the state stays as it
    /// was then.
    pub fn set_muted(&self, muted: bool) -> Result<(), FfiError> {
        self.inner.set_muted(muted)
    }

    pub fn set_observer(self: &Arc<Self>, observer: Arc<dyn PlayerObserver>) {
        self.inner.set_observer(observer);
    }

    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] for a rate that is not a finite
    /// number, and the player's refusal of the new rate; the playing rate
    /// stays as it was then.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_playing_rate(&self, rate: f32) -> Result<(), FfiError> {
        self.inner.set_playing_rate(rate)
    }

    /// Change the queue repeat mode.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if `mode` has no queue-level
    /// meaning.
    pub fn set_repeat_mode(&self, mode: FfiRepeatMode) -> Result<(), FfiError> {
        self.inner.set_repeat_mode(mode)
    }

    /// # Errors
    ///
    /// Returns the player's refusal of the change; the volume stays as it
    /// was then.
    pub fn set_volume(&self, volume: f32) -> Result<(), FfiError> {
        self.inner.set_volume(volume)
    }

    /// Register a runtime DRM key processor for every host (`"*"`).
    ///
    /// Generates a fresh 16-character alphanumeric `salt`, mirrors it
    /// into the player-wide `SALT_HEADER` (so it accompanies every
    /// outgoing manifest/segment/key request), and forwards it to
    /// `processor.process_key(key, salt)` on each decrypt.
    ///
    /// Items already in the queue keep their original key registry —
    /// re-call this method *before* [`Self::insert`] for the new processor
    /// to apply.
    pub fn setup_hls_aes(&self, processor: Arc<dyn FfiKeyProcessor>) {
        self.inner.setup_hls_aes(processor);
    }

    /// Register a runtime DRM key processor with explicit rule control
    /// (custom domains, headers, salt). The rule's salt — if any — is
    /// mirrored into the player-wide header map under `SALT_HEADER`.
    ///
    /// Items already in the queue keep their original key registry.
    pub fn setup_hls_aes_with_rule(&self, rule: FfiKeyRule) {
        self.inner.setup_hls_aes_with_rule(rule);
    }

    /// Player-wide auth header. Stores `auth_token` under
    /// `AUTH_TOKEN_HEADER`; merged into per-item HTTP headers on
    /// every subsequent [`Self::insert`]. Pass an empty string to clear.
    pub fn setup_network(&self, auth_token: String) {
        self.inner.setup_network(auth_token);
    }

    /// Return a snapshot of the player's current state.
    #[must_use]
    pub fn snapshot(&self) -> FfiPlayerSnapshot {
        self.inner.snapshot()
    }

    /// Stop playback: pause the engine and reset the current item's
    /// position to the start. The queue is preserved, so a subsequent
    /// [`play`](Self::play) resumes the same item from the beginning.
    /// To empty the queue instead, use [`remove_all_items`](Self::remove_all_items).
    pub fn stop(&self) {
        self.inner.stop();
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
        self.inner.update_peak_bitrate(wifi_bps, cellular_bps);
    }

    pub fn volume(&self) -> f32 {
        self.inner.volume()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara::platform::sync::mpsc::{Receiver, Sender, channel};

    use super::*;
    use crate::types::FfiPlayerEvent;

    struct ChannelObserver {
        sender: Sender<FfiPlayerEvent>,
    }

    impl PlayerObserver for ChannelObserver {
        fn on_event(&self, event: FfiPlayerEvent) {
            self.sender.send(event).ok();
        }
    }

    /// Blocks on the fact rather than on a deadline: the observer runs on the
    /// event-bridge thread, so a local timeout would race it. A mode change
    /// that never arrives is a hang, and the harness bounds hangs.
    #[kithara::allow_block]
    fn received_repeat_mode(receiver: &Receiver<FfiPlayerEvent>, expected: FfiRepeatMode) -> bool {
        while let Ok(event) = receiver.recv() {
            if matches!(
                event,
                FfiPlayerEvent::RepeatModeChanged { mode } if mode == expected
            ) {
                return true;
            }
        }
        false
    }

    #[kithara::test]
    fn repeat_mode_round_trips_and_notifies_observer() {
        let player = AudioPlayer::new(FfiPlayerConfig::for_test()).expect("create player");
        let (sender, receiver) = channel();
        player.set_observer(Arc::new(ChannelObserver { sender }));

        player
            .set_repeat_mode(FfiRepeatMode::All)
            .expect("valid repeat mode");

        assert_eq!(player.repeat_mode(), FfiRepeatMode::All);
        assert!(received_repeat_mode(&receiver, FfiRepeatMode::All));
        drop(player);
    }
}
