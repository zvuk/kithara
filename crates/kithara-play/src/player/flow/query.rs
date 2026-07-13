use delegate::delegate;
use kithara_audio::{AudioWorkerHandle, EngineLoadSnapshot};
use kithara_events::EventBus;
use kithara_platform::tokio::runtime::Handle as RuntimeHandle;

use super::super::core::PlayerImpl;
use crate::{api::PlayerStatus, bridge::PlaybackSnapshot, engine::EngineImpl};

impl PlayerImpl {
    delegate! {
        to self.core.params {
            /// Whether the built-in linear auto-advance handler is enabled.
            #[must_use]
            pub fn auto_advance_enabled(&self) -> bool;
            /// Get crossfade duration in seconds.
            pub fn crossfade_duration(&self) -> f32;
            /// Default playback rate used by `play()` and `select_item()`.
            pub fn default_rate(&self) -> f32;
            /// Returns `true` if the player is muted.
            pub fn is_muted(&self) -> bool;
            /// Get prefetch lead time in seconds.
            pub fn prefetch_duration(&self) -> f32;
            /// Current playback rate (0.0 = paused).
            pub fn rate(&self) -> f32;
            /// Get current volume (0.0..=1.0).
            pub fn volume(&self) -> f32;
        }
        to self.core.engine {
            /// Root event bus for this player.
            #[must_use]
            pub fn bus(&self) -> &EventBus;
            /// Number of EQ bands available for this player.
            pub fn eq_band_count(&self) -> usize;
            /// Runtime handle captured by this player's engine.
            #[must_use]
            pub fn runtime(&self) -> Option<&RuntimeHandle>;
            /// Shared audio worker handle for this player's engine.
            #[must_use]
            pub fn worker(&self) -> &AudioWorkerHandle;
        }
    }

    /// Current item index in the queue.
    pub fn current_index(&self) -> usize {
        self.core.items.current_index()
    }

    /// ABR handle of the currently loaded item, if any.
    ///
    /// Reads the stash populated by `enqueue_to_processor` — stays valid for
    /// the whole life of the track, including after `items[idx]` has been
    /// emptied by the load handoff.
    #[must_use]
    pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.phase.lock().abr_handle()
    }

    /// Get a reference to the underlying engine.
    pub fn engine(&self) -> &EngineImpl {
        &self.core.engine
    }

    /// Live cost snapshot of the audio engine (decode + effects).
    #[must_use]
    pub fn engine_load(&self) -> EngineLoadSnapshot {
        self.core.engine_load.snapshot()
    }

    /// Current media duration in seconds.
    ///
    /// Returns `None` while duration is unknown — the engine sets the shared
    /// atomic from the demuxer once mvhd / fmt-equivalent metadata is parsed.
    /// The atomic's default `0.0` conflates "unknown" with "empty track";
    /// callers that distinguish (e.g. `seek_seconds`'s `target >= dur` check,
    /// queue auto-advance) need the `None` to avoid false-EOF on a freshly-
    /// loaded track whose demuxer has not yet seen the metadata box.
    pub fn duration_seconds(&self) -> Option<f64> {
        let dur = self.playback_snapshot()?.duration;
        (dur > 0.0).then_some(dur)
    }

    /// Get EQ gain for a band in dB.
    pub fn eq_gain(&self, band: usize) -> Option<f32> {
        let slot_id = self.slot()?;
        self.core
            .engine
            .slot_eq(slot_id)
            .and_then(|eq| eq.gain(band))
    }

    /// Returns `true` if the player is in playing state.
    pub fn is_playing(&self) -> bool {
        self.playback_snapshot().is_some_and(|s| s.playing)
    }

    /// Get the number of items in the queue (including consumed items).
    pub fn item_count(&self) -> usize {
        self.core.items.item_count()
    }

    /// Single coherent read of the active slot's live playback scalars.
    ///
    /// `None` when no slot is allocated. The standalone `position_seconds`
    /// / `duration_seconds` / `is_playing` / `buffered_seconds` getters are
    /// thin derivations of this snapshot — one shared read primitive.
    pub fn playback_snapshot(&self) -> Option<PlaybackSnapshot> {
        let slot_id = self.slot()?;
        Some(self.core.engine.slot_playback(slot_id)?.snapshot())
    }

    /// Current playback position in seconds.
    pub fn position_seconds(&self) -> Option<f64> {
        Some(self.playback_snapshot()?.position)
    }

    /// Get current player status.
    pub fn status(&self) -> PlayerStatus {
        *self.core.status.lock()
    }

    /// Subscribe to player events.
    pub fn subscribe(&self) -> kithara_events::EventReceiver {
        self.core.engine.bus().subscribe()
    }
}
