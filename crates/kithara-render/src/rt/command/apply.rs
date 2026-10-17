use kithara_command::{Due, Seq, Target};
use kithara_config::LiveConfig;
use kithara_events::TrackId;
use kithara_signal::{SegmentId, SessionFrame};
#[cfg(feature = "usdt")]
use kithara_test_macros as kithara;

use super::{
    super::{
        processor::Deck,
        track::{PlayerResource, PlayerTrack},
    },
    state::*,
};
use crate::bridge::{DeckEqChange, DeckPart, DeckProtocol, Fade, Returned, Slot};

impl Deck {
    pub(in crate::rt) fn apply(
        &mut self,
        part: DeckPart,
        at: SessionFrame,
        seq: Seq,
        context_valid: bool,
    ) -> Option<DeckPart> {
        match part {
            DeckPart::Attach {
                slot,
                item_id,
                pcm,
                segment,
            } => self.attach(slot, item_id, pcm, segment, at, seq),
            DeckPart::Detach { slot } => self.detach(slot, context_valid),
            DeckPart::Start { slot, fade } => {
                if let Some(track) = self.tracks.at_mut(slot) {
                    track.start(fade);
                }
                Some(DeckPart::Start { slot, fade })
            }
            DeckPart::Stop { slot, fade } => self.stop_track(slot, fade, at, seq),
            DeckPart::Adopt { slot, segment } => {
                Some(self.adopt_track(slot, segment, at, seq, context_valid))
            }
            DeckPart::Fade {
                slot,
                settings,
                dir,
            } => {
                if let Some(track) = self.tracks.at_mut(slot) {
                    track.fade(settings, dir);
                }
                Some(DeckPart::Fade {
                    slot,
                    settings,
                    dir,
                })
            }
            DeckPart::Replace {
                slot,
                item_id,
                pcm,
                segment,
            } => Some(self.replace_track(slot, item_id, pcm, segment, context_valid)),
            DeckPart::Chain { from, to } => Some(self.chain_tracks(from, to)),
            DeckPart::Mix(change) => {
                self.mix.apply_change(change);
                self.render.set_gain(self.mix.gain());
                Some(DeckPart::Mix(change))
            }
            DeckPart::Eq(DeckEqChange::Gain { band, gain }) => {
                self.render.set_eq_gain(band, gain);
                Some(DeckPart::Eq(DeckEqChange::Gain { band, gain }))
            }
            DeckPart::Eq(DeckEqChange::Layout(layout)) => self
                .render
                .take_eq_layout(layout)
                .map(|layout| DeckPart::Returned(Returned::Eq(layout))),
            DeckPart::Returned(returned) => Some(DeckPart::Returned(returned)),
        }
    }

    fn chain_tracks(&mut self, from: Slot, to: Slot) -> DeckPart {
        if let Some(track) = self.tracks.at_mut(to) {
            track.start(Fade::Crossfade(crate::CrossfadeSettings {
                duration: 0.0,
                ..Default::default()
            }));
            track.snap_gate();
        }
        DeckPart::Chain { from, to }
    }

    fn attach(
        &mut self,
        slot: Slot,
        item_id: TrackId,
        pcm: Box<PlayerResource>,
        segment: SegmentId,
        at: SessionFrame,
        seq: Seq,
    ) -> Option<DeckPart> {
        #[cfg(not(feature = "usdt"))]
        let _ = (at, seq);
        let track = self.track(pcm, segment, item_id);
        match self.tracks.put(slot, track) {
            Ok(()) => {
                #[cfg(feature = "usdt")]
                {
                    let track_id = item_id.as_u64();
                    let slot = u32::from(slot.get());
                    let segment = segment.get();
                    let at = i64::from(at);
                    let seq = seq.get();
                    kithara::probe_event!(deck_attached, track_id, slot, segment, at, seq);
                }
                None
            }
            Err(resource) => Some(returned_pcm(slot, resource)),
        }
    }

    fn detach(&mut self, slot: Slot, context_valid: bool) -> Option<DeckPart> {
        let mut track = self.tracks.take(slot)?;
        if context_valid {
            self.tails[slot.index()].fill(
                &mut track,
                self.declick_frames,
                &self.metrics,
                &mut self.recycle[slot.index()],
            );
        }
        Some(returned_pcm(slot, track.into_resource()))
    }

    fn stop_track(
        &mut self,
        slot: Slot,
        fade: Fade,
        at: SessionFrame,
        seq: Seq,
    ) -> Option<DeckPart> {
        let track = self.tracks.at_mut(slot)?;
        track.stop(seq, fade, at);
        if let Some((_, resume)) = track.take_stop() {
            Some(DeckPart::Returned(Returned::Stopped { slot, resume }))
        } else {
            Some(DeckPart::Stop { slot, fade })
        }
    }

    fn adopt_track(
        &mut self,
        slot: Slot,
        segment: SegmentId,
        at: SessionFrame,
        seq: Seq,
        context_valid: bool,
    ) -> DeckPart {
        #[cfg(not(feature = "usdt"))]
        let _ = (at, seq);
        if let Some(track) = self.tracks.at_mut(slot) {
            if context_valid {
                self.tails[slot.index()].fill(
                    track,
                    self.declick_frames,
                    &self.metrics,
                    &mut self.recycle[slot.index()],
                );
            }
            track.adopt(segment);
            #[cfg(feature = "usdt")]
            {
                let slot = u32::from(slot.get());
                let segment = segment.get();
                let at = i64::from(at);
                let seq = seq.get();
                kithara::probe_event!(deck_adopted, slot, segment, at, seq);
            }
            track.recycle_obsolete(&mut self.recycle[slot.index()]);
        }
        DeckPart::Adopt { slot, segment }
    }

    fn replace_track(
        &mut self,
        slot: Slot,
        item_id: TrackId,
        pcm: Box<PlayerResource>,
        segment: SegmentId,
        context_valid: bool,
    ) -> DeckPart {
        let mut track = self.track(pcm, segment, item_id);
        track.start(Fade::Declick);
        track.snap_gate();
        match self.tracks.replace(slot, track) {
            Ok(mut old) => {
                if old.state() == crate::bridge::SlotState::Playing {
                    self.metrics.record_evicted_playing();
                }
                if context_valid {
                    self.tails[slot.index()].fill(
                        &mut old,
                        self.evict_frames,
                        &self.metrics,
                        &mut self.recycle[slot.index()],
                    );
                }
                returned_pcm(slot, old.into_resource())
            }
            Err(resource) => returned_pcm(slot, resource),
        }
    }

    pub(in crate::rt) fn track(
        &self,
        pcm: Box<PlayerResource>,
        segment: SegmentId,
        item_id: TrackId,
    ) -> PlayerTrack {
        PlayerTrack::builder()
            .item_id(item_id)
            .sample_rate(self.sample_rate)
            .declick(self.declick)
            .segment(segment)
            .build(pcm)
    }

    pub(in crate::rt) fn interrupt_stop(
        &mut self,
        due: &mut Due<'_, DeckProtocol>,
        slot: Slot,
        remaining: usize,
    ) {
        let Some(track) = self.tracks.at_mut(slot) else {
            return;
        };
        track.interrupt_stop(due.at());
        let Some((seq, resume)) = track.take_stop() else {
            return;
        };
        if seq == due.seq() {
            answer_stop(&mut due.commands_mut()[remaining..], slot, resume);
        } else if due
            .committed_mut(seq)
            .is_some_and(|commands| answer_stop(commands, slot, resume))
        {
            due.complete(seq, ());
        }
    }
}
