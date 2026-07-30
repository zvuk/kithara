use std::sync::PoisonError;

use kithara_events::{
    AdvanceReason, AudioEvent, Envelope, Event, ItemEvent, PlayerEvent, QueueEvent, TrackId,
    TrackStatus,
};
use kithara_platform::{sync::Arc, tokio::sync::broadcast::error::TryRecvError};
use tracing::debug;

use super::{
    Queue,
    types::{CachedPosition, CrossfadeArm, Transition},
};
use crate::track::TrackSource;

impl Queue {
    pub(super) fn advance_loaded_successor(&self, current_id: TrackId, transition: Transition) {
        let Some(next) = self.next_selectable_entry() else {
            return;
        };
        if !matches!(next.status, TrackStatus::Loaded) {
            return;
        }

        let before_index = self.player.current_index();
        if self
            .select_with_reason(next.id, transition, AdvanceReason::CrossfadePreArm)
            .is_err()
        {
            return;
        }
        if self.player.current_index() != before_index {
            self.write_armed_for(CrossfadeArm::armed(current_id));
        }
    }

    /// If an advance was already armed from `tick()`, consume it and
    /// return `true` — the engine's trailing `ItemDidPlayToEnd` for
    /// the same track must not advance again.
    pub(super) fn consume_armed_advance(
        &self,
        ended_id: Option<TrackId>,
        pos: f64,
        dur: f64,
    ) -> bool {
        let Some(ended_id) = ended_id else {
            return false;
        };
        if self.take_armed_for_if_matches(ended_id) {
            debug!(
                track_id = ended_id.as_u64(),
                pos, dur, "consumed ItemDidPlayToEnd (armed pre-end)"
            );
            true
        } else {
            false
        }
    }

    /// Either treat the EOF as a real end-of-track and advance, or log
    /// it as a spurious signal (decoder-failure pos stamp, crossfade
    /// fade-out on previous track).
    pub(super) fn dispatch_real_or_spurious(&self, pos: f64, dur: f64) {
        /// Threshold for filtering spurious `PlayerEvent::ItemDidPlayToEnd`
        /// events emitted by crossfade fade-outs of non-current tracks.
        const ITEM_END_POSITION_TOLERANCE_SECONDS: f64 = 1.0;

        if dur > 0.0 && pos >= dur - ITEM_END_POSITION_TOLERANCE_SECONDS {
            let _ = self.advance_to_next(Transition::Crossfade, AdvanceReason::NaturalEof);
        } else {
            debug!(pos, dur, "filtered spurious ItemDidPlayToEnd");
        }
    }

    pub(super) fn drain_player_events(&self) {
        let mut rx = self
            .player_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            match rx.try_recv() {
                Ok(Envelope { event: ev, .. }) => self.process_player_event(&ev),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
    }

    pub(super) fn handle_current_item_changed(&self) {
        let idx = self.player.current_index();
        let id = self.lock_tracks().get(idx).map(|e| e.id);
        self.write_cached_position(CachedPosition::Unknown);
        self.bus.publish(QueueEvent::CurrentTrackChanged { id });
    }

    pub(super) fn handle_handover_requested(&self) {
        if self.is_paused() {
            return;
        }
        let Some(entry) = self.current() else {
            return;
        };
        self.advance_loaded_successor(entry.id, Transition::Crossfade);
    }

    pub(super) fn handle_item_did_fail(&self, src: &Arc<str>) {
        let snap = self.player.playback_snapshot();
        let pos = snap.map_or(0.0, |s| s.position());
        let dur = snap.map_or(0.0, |s| s.duration());
        debug!(%src, pos, dur, "ItemDidFail received — track aborted mid-stream");
        if self.current().is_none() {
            self.bus.publish(QueueEvent::QueueEnded);
            return;
        }
        if self.is_paused() {
            debug!(%src, "paused: not auto-advancing on ItemDidFail");
            return;
        }
        let ended_id = self.track_id_for_src(src);
        if self.consume_armed_advance(ended_id, pos, dur) {
            return;
        }
        if let Some(id) = ended_id {
            self.set_status(
                id,
                TrackStatus::Failed("mid-stream engine failure".to_string()),
            );
            self.bus.publish(QueueEvent::TrackLoadFailed {
                id,
                reason: "mid-stream engine failure".to_string(),
                auto_skipped: true,
            });
        }
        let _ = self.advance_to_next(Transition::None, AdvanceReason::TrackFailed);
    }

    /// Decide whether `ItemDidPlayToEnd` advances the queue or is
    /// filtered as a stale crossfade fade-out signal.
    ///
    /// `src` identifies the underlying audio source of the track that
    /// just hit EOF. The player only emits `TrackPlaybackStopped` from
    /// its natural-EOF path (`handle_eof`), so a non-empty `src` is the
    /// authoritative end-of-track signal: advance unconditionally
    /// (subject to crossfade pre-arm consumption).
    ///
    /// The empty-`src` arm preserves backward compatibility for
    /// pre-PR-#64 callers and acts as a defensive fallback when the
    /// player has not yet wired the src through; it falls back to the
    /// pos/dur tolerance heuristic to filter spurious events.
    pub(super) fn handle_item_did_play_to_end(&self, src: &Arc<str>) {
        let snap = self.player.playback_snapshot();
        let pos = snap.map_or(0.0, |s| s.position());
        let dur = snap.map_or(0.0, |s| s.duration());
        debug!(%src, pos, dur, "ItemDidPlayToEnd received");
        if self.current().is_none() {
            self.bus.publish(QueueEvent::QueueEnded);
            return;
        }
        if self.is_paused() {
            debug!(%src, pos, dur, "paused: not auto-advancing on ItemDidPlayToEnd");
            return;
        }
        let ended_id = self.track_id_for_src(src);
        if self.consume_armed_advance(ended_id, pos, dur) {
            return;
        }
        if src.is_empty() {
            self.dispatch_real_or_spurious(pos, dur);
        } else {
            let _ = self.advance_to_next(Transition::Crossfade, AdvanceReason::NaturalEof);
        }
    }

    pub(super) fn process_player_event(&self, ev: &Event) {
        match ev {
            Event::Player(PlayerEvent::ItemDidPlayToEnd { src, .. }) => {
                self.handle_item_did_play_to_end(src);
            }
            Event::Player(PlayerEvent::ItemDidFail { src, .. }) => {
                self.handle_item_did_fail(src);
            }
            Event::Player(PlayerEvent::CurrentItemChanged) => {
                self.handle_current_item_changed();
            }
            Event::Player(PlayerEvent::HandoverRequested) => {
                self.handle_handover_requested();
            }
            Event::Audio(AudioEvent::UnderrunStarted { .. }) => {
                self.bus.publish(ItemEvent::PlaybackStalled);
            }
            Event::Audio(AudioEvent::UnderrunEnded { .. }) => {
                self.bus.publish(ItemEvent::PlaybackLikelyToKeepUp);
            }
            _ => {}
        }
    }

    pub(super) fn track_id_for_src(&self, src: &str) -> Option<TrackId> {
        self.lock_tracks().iter().find_map(|record| {
            let matches = match &record.source {
                TrackSource::Uri(uri) => uri == src,
                TrackSource::Config(config) => config.source().to_string() == src,
            };
            matches.then_some(record.id)
        })
    }
}
