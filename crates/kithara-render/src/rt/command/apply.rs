use kithara_command::{Due, Seq, Target};
use kithara_events::TrackId;
use kithara_signal::{SegmentId, SessionFrame};
#[cfg(feature = "usdt")]
use kithara_test_macros as kithara;

use super::{
    super::{
        processor::Deck,
        track::{PlayerTrack, ReadPcm},
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
        read_pcm: ReadPcm,
    ) -> Option<DeckPart> {
        self.interrupt_underrun(&part, at);
        match part {
            DeckPart::Attach {
                slot,
                item_id,
                pcm,
                segment,
            } => self.attach(slot, item_id, pcm, segment, at, seq),
            DeckPart::Detach { slot } => self.detach(slot, context_valid, read_pcm),
            DeckPart::Start { slot, fade } => {
                if let Some(track) = self.tracks.at_mut(slot) {
                    track.start(fade);
                }
                Some(DeckPart::Start { slot, fade })
            }
            DeckPart::Stop { slot, fade } => self.stop_track(slot, fade, at, seq),
            DeckPart::Adopt { slot, segment } => {
                Some(self.adopt_track(slot, segment, at, seq, context_valid, read_pcm))
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
            } => {
                let track = self.track(pcm, segment, item_id, seq);
                Some(self.replace_track(slot, track, context_valid, read_pcm))
            }
            DeckPart::Chain { from, to } => Some(self.chain_tracks(from, to)),
            DeckPart::Clock { slot, clock } => {
                if let Some(track) = self.tracks.at_mut(slot) {
                    track.set_clock(clock);
                }
                Some(DeckPart::Clock { slot, clock })
            }
            DeckPart::Mix(change) => {
                self.render.set_mix(change);
                Some(DeckPart::Mix(change))
            }
            DeckPart::Eq(DeckEqChange::Gain { band, gain }) => {
                self.render.set_eq_gain(band, gain);
                Some(DeckPart::Eq(DeckEqChange::Gain { band, gain }))
            }
            DeckPart::Eq(DeckEqChange::Kill { band, layout, on }) => {
                self.render.set_eq_kill(band, on);
                Some(DeckPart::Eq(DeckEqChange::Kill { band, layout, on }))
            }
            DeckPart::Eq(DeckEqChange::Reset) => {
                self.render.reset_eq();
                Some(DeckPart::Eq(DeckEqChange::Reset))
            }
            DeckPart::Eq(DeckEqChange::Layout(layout)) => self
                .render
                .take_eq_layout(layout)
                .map(|layout| DeckPart::Returned(Returned::Eq(layout))),
            DeckPart::Program { program, .. } => {
                self.programs.begin(program.parameter(), seq);
                Some(DeckPart::Returned(Returned::Program(program)))
            }
            DeckPart::Fx(change) => self.render.change_fx(change, self.sample_rate),
            DeckPart::GainProgram {
                slot,
                binding,
                authority,
                program,
            } => {
                self.tracks
                    .at_mut(slot)
                    .expect("validated output target")
                    .begin_output(seq, program.curve(), at);
                Some(DeckPart::GainProgram {
                    slot,
                    binding,
                    authority,
                    program,
                })
            }
            DeckPart::Returned(returned) => Some(DeckPart::Returned(returned)),
        }
    }

    fn interrupt_underrun(&mut self, part: &DeckPart, at: SessionFrame) {
        match part {
            DeckPart::Start { slot, .. }
            | DeckPart::Stop { slot, .. }
            | DeckPart::Adopt { slot, .. }
            | DeckPart::Detach { slot }
            | DeckPart::Replace { slot, .. } => self.finish_underrun(*slot, at),
            DeckPart::Chain { to, .. } => self.finish_underrun(*to, at),
            _ => {}
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
        pcm: Box<crate::PcmReceiver>,
        segment: SegmentId,
        at: SessionFrame,
        seq: Seq,
    ) -> Option<DeckPart> {
        #[cfg(not(feature = "usdt"))]
        let _ = (at, seq);
        let track = self.track(pcm, segment, item_id, seq);
        if let Some(track) = self.tracks.put(slot, track) {
            return Some(returned_pcm(slot, track));
        }
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

    fn detach(&mut self, slot: Slot, context_valid: bool, read_pcm: ReadPcm) -> Option<DeckPart> {
        let mut track = self.tracks.take(slot)?;
        if context_valid {
            self.tails[slot.index()].fill(
                &mut track,
                self.declick_frames,
                &mut self.recycle[slot.index()],
                read_pcm,
            );
        }
        Some(returned_pcm(slot, track))
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
        read_pcm: ReadPcm,
    ) -> DeckPart {
        #[cfg(not(feature = "usdt"))]
        let _ = (at, seq);
        if let Some(track) = self.tracks.at_mut(slot) {
            if context_valid {
                self.tails[slot.index()].fill(
                    track,
                    self.declick_frames,
                    &mut self.recycle[slot.index()],
                    read_pcm,
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
        mut track: PlayerTrack,
        context_valid: bool,
        read_pcm: ReadPcm,
    ) -> DeckPart {
        track.start(Fade::Declick);
        track.snap_gate();
        let Some(current) = self.tracks.at_mut(slot) else {
            return returned_pcm(slot, track);
        };
        let mut old = std::mem::replace(current, track);
        if old.state() == crate::bridge::SlotState::Playing {
            self.metrics.record_evicted_playing();
        }
        if context_valid {
            self.tails[slot.index()].fill(
                &mut old,
                self.evict_frames,
                &mut self.recycle[slot.index()],
                read_pcm,
            );
        }
        returned_pcm(slot, old)
    }

    pub(in crate::rt) fn track(
        &self,
        pcm: Box<crate::PcmReceiver>,
        segment: SegmentId,
        item_id: TrackId,
        seq: Seq,
    ) -> PlayerTrack {
        PlayerTrack::builder()
            .item_id(item_id)
            .sample_rate(self.sample_rate)
            .declick(self.declick)
            .segment(segment)
            .attachment(seq)
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
        } else if due.committed_mut(seq).is_some_and(|commands| {
            answer_stop(commands, slot, resume) && self.output_finished(commands, seq)
        }) {
            due.complete(seq, ());
        }
    }
}
