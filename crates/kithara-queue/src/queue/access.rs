use kithara_audio::AudioObserver;
use kithara_bufpool::HasPool;
use kithara_events::{EventReceiver, EventSet, TrackId};
use smallvec::SmallVec;

use super::QueueControl;
use crate::{
    event::{QueueEvent, QueueRepeatMode},
    navigation::{ActionAtItemEnd, PlaybackOrder, RepeatMode},
    track::{TrackEntry, TrackRecord, TrackSource},
};

impl<S> QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    #[must_use]
    pub fn action_at_item_end(&self) -> ActionAtItemEnd {
        self.config.action_at_item_end()
    }

    /// The currently playing track entry, if any.
    ///
    /// Sourced from the navigation cursor (not the player) so the queue
    /// reports `None` after `advance_to_next` runs off the end of the
    /// queue (`RepeatMode::Off` exhaustion). The player's own
    /// `current_index` stays parked at the last-played slot — read it
    /// via [`Self::current_index`] when the call site needs the
    /// last-played index even after queue-end.
    #[must_use]
    pub fn current(&self) -> Option<TrackEntry> {
        let id = self.lock_navigation().current()?;
        self.track(id)
    }

    /// The currently playing track's queue index (player-reported).
    #[must_use]
    pub fn current_index(&self) -> Option<usize> {
        let idx = self.player.current_index();
        if idx < self.len() { Some(idx) } else { None }
    }

    pub fn set_action_at_item_end(&self, action: ActionAtItemEnd) {
        self.command(|queue| {
            queue.config.set_action_at_item_end(action);
            queue
                .bus
                .publish(QueueEvent::ActionAtItemEndChanged { action });
        });
    }

    pub fn set_playback_order(&self, order: PlaybackOrder) {
        self.command(|queue| {
            let ids = queue
                .tracks()
                .into_iter()
                .map(|track| track.id)
                .collect::<SmallVec<[_; 16]>>();
            queue.lock_navigation_mut().set_playback_order(order, &ids);
            queue
                .bus
                .publish(QueueEvent::PlaybackOrderChanged { order });
        });
    }

    /// Set repeat mode.
    pub fn set_repeat(&self, mode: RepeatMode) {
        self.command(|queue| {
            queue.lock_navigation_mut().set_repeat(mode);
            queue.bus.publish(QueueEvent::RepeatModeChanged {
                mode: map_repeat_mode(mode),
            });
        });
    }

    /// Subscribe to the unified event stream:
    /// [`QueueEvent`](crate::event::QueueEvent) + underlying player /
    /// audio / hls / file events.
    #[must_use]
    pub fn subscribe<E: EventSet>(&self) -> EventReceiver<E> {
        self.bus.subscribe()
    }

    /// Lookup a track entry by id.
    #[must_use]
    pub fn track(&self, id: TrackId) -> Option<TrackEntry> {
        self.lock_tracks()
            .iter()
            .find(|r| r.id == id)
            .map(TrackRecord::entry)
    }

    /// The original [`TrackSource`] for `id`, if still queued. Lets callers
    /// rebuild a resource by track identity rather than by queue position.
    #[must_use]
    pub fn track_source(&self, id: TrackId) -> Option<TrackSource<S>> {
        self.tracks.source(id)
    }

    delegate::delegate! {
        to self.loader {
            /// Attach a bounded decoded-audio observer to `id`'s decoder.
            ///
            /// Attachment is nonblocking and works before, during, or after resource
            /// loading. Only one observer is active for a track at a time.
            pub fn attach_observer<O: AudioObserver>(&self, id: TrackId, observer: O);
        }
        to self.player {
            /// ABR handle of the currently playing adaptive item, if any.
            ///
            /// Returned handle drives runtime variant/bandwidth control — FFI and
            /// GUI use it for `set_abr_mode` / `set_preferred_peak_bitrate`.
            #[must_use]
            pub fn current_abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            /// Rate the player's master bus runs at, and therefore the frame axis used
            /// by decoded-audio observers attached to this queue.
            #[must_use]
            pub fn sample_rate(&self) -> u32;
        }
        to self {
            /// Live variant metadata of the currently playing adaptive item.
            /// Pulled from the player's stashed ABR handle on every call so a
            /// renderer can poll for the up-to-date label after every frame
            /// without depending on event delivery.
            #[must_use]
            #[expr($?.current_variant())]
            #[call(current_abr_handle)]
            pub fn current_variant(&self) -> Option<kithara_abr::VariantInfo>;
            /// Whether the queue is empty.
            #[must_use]
            #[expr($.is_empty())]
            #[call(lock_tracks)]
            pub fn is_empty(&self) -> bool;
            /// Current traversal order.
            #[must_use]
            #[expr($.playback_order())]
            #[call(lock_navigation)]
            pub fn playback_order(&self) -> PlaybackOrder;
            /// Number of tracks currently in the queue.
            #[must_use]
            #[expr($.len())]
            #[call(lock_tracks)]
            pub fn len(&self) -> usize;
            /// Current repeat mode.
            #[must_use]
            #[expr($.repeat_mode())]
            #[call(lock_navigation)]
            pub fn repeat_mode(&self) -> RepeatMode;
            /// Snapshot of all track entries, in queue order.
            #[must_use]
            #[expr($.iter().map(TrackRecord::entry).collect())]
            #[call(lock_tracks)]
            pub fn tracks(&self) -> Vec<TrackEntry>;
        }
    }
}

const fn map_repeat_mode(mode: RepeatMode) -> QueueRepeatMode {
    match mode {
        RepeatMode::Off => QueueRepeatMode::Off,
        RepeatMode::One => QueueRepeatMode::One,
        RepeatMode::All => QueueRepeatMode::All,
    }
}
