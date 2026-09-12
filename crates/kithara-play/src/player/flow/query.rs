use delegate::delegate;
use kithara_events::{EventBus, EventReceiver, EventSet};
use kithara_platform::tokio::runtime::Handle as RuntimeHandle;

use super::super::core::PlayerRuntime;
use crate::{
    EngineLoadSnapshot, PlayWorker,
    api::PlayerStatus,
    bridge::{PlaybackSnapshot, RtMetricsSnapshot},
    engine::EngineImpl,
};

impl<S> PlayerRuntime<S> {
    /// ABR handle of the currently loaded item, if any.
    ///
    /// Reads the stash populated by `enqueue_to_processor` — stays valid for
    /// the whole life of the track, including after `items[idx]` has been
    /// emptied by the load handoff.
    #[must_use]
    pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.phase.lock().abr_handle()
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

    /// Live cost snapshot of the audio engine (decode + effects).
    #[must_use]
    pub fn engine_load(&self) -> EngineLoadSnapshot {
        self.core.engine_load.snapshot()
    }

    /// Get EQ gain for a band in dB.
    pub fn eq_gain(&self, band: usize) -> Option<f32> {
        self.core.engine.eq().and_then(|eq| eq.gain(band))
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
    ///
    /// The media clock owns the answer once a slot carries the track. Before
    /// that there is no clock to ask, and the only truth about the current
    /// item's playhead is the position handed over for it to start at: a host
    /// restoring a stored position reads it back here to draw its progress,
    /// and answering `None` is what puts the scrubber at the head of a track
    /// the player has already accepted a seek for.
    #[must_use]
    pub fn position_seconds(&self) -> Option<f64> {
        if let Some(snapshot) = self.playback_snapshot() {
            return Some(snapshot.position);
        }
        self.core
            .start_position
            .lock()
            .map(|held| held.as_secs_f64())
    }

    /// Read the active audio slot's real-time counters.
    #[must_use]
    pub fn rt_metrics(&self) -> Option<RtMetricsSnapshot> {
        let slot_id = self.slot()?;
        Some(
            self.core
                .engine
                .slot_playback(slot_id)?
                .metrics()
                .snapshot(),
        )
    }

    /// Get current player status.
    pub fn status(&self) -> PlayerStatus {
        *self.core.status.lock()
    }

    /// Subscribe to player events.
    pub fn subscribe<E: EventSet>(&self) -> EventReceiver<E> {
        self.core.engine.bus().subscribe()
    }

    delegate! {
        to self.core {
            /// Get a reference to the underlying engine.
            #[field(&engine)]
            pub const fn engine(&self) -> &EngineImpl<S>;
            /// Shared playback worker configured for this Player.
            #[field(&worker)]
            #[must_use]
            pub const fn worker(&self) -> &PlayWorker<S>;
        }
        to self.core.params {
            /// Whether the built-in linear auto-advance handler is enabled.
            #[must_use]
            pub fn auto_advance_enabled(&self) -> bool;
            /// Get crossfade duration in seconds.
            pub fn crossfade_duration(&self) -> f32;
            /// Default playback-rate target used by `play()` and `select_item()`.
            pub fn default_rate(&self) -> f32;
            /// Returns `true` if the player is muted.
            pub fn is_muted(&self) -> bool;
            /// Get prefetch lead time in seconds.
            pub fn prefetch_duration(&self) -> f32;
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
            pub const fn runtime(&self) -> Option<&RuntimeHandle>;
        }
        to self {
            /// Returns `true` if the player is in playing state.
            #[expr($.is_some_and(|s| s.playing))]
            #[call(playback_snapshot)]
            pub fn is_playing(&self) -> bool;
            /// Current effective playback rate (`0.0` while paused or without a slot).
            #[expr($.map_or(0.0, |snapshot| snapshot.rate))]
            #[call(playback_snapshot)]
            pub fn rate(&self) -> f32;
        }
        to self.core.items {
            /// Current item index in the queue.
            pub fn current_index(&self) -> usize;
            /// Get the number of items in the queue (including consumed items).
            pub fn item_count(&self) -> usize;
            /// Whether the queue slot at `index` still holds a resource.
            ///
            /// Loading an item into the processor empties its slot, so this is the
            /// owning answer to "has this item been consumed" — the same fact
            /// [`select_item`](Self::select_item) refuses to guess at. Callers that
            /// mirror item state read it here instead of inferring the consumption
            /// from their own bookkeeping.
            #[must_use]
            #[call(has_resource)]
            pub fn item_has_resource(&self, index: usize) -> bool;
        }
    }
}
