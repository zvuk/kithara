use kithara_command::{Due, Seq, Target};
use kithara_config::LiveConfig;
use kithara_events::TrackId;
use kithara_signal::{SegmentId, SessionFrame};
#[cfg(feature = "usdt")]
use kithara_test_macros as kithara;

use super::{
    super::{
        processor::Deck,
        track::{PlayerTrack, ReadPcm},
    },
    state::{answer_stop, returned_pcm},
};
use crate::bridge::{
    DeckEqChange, DeckMixSettingsChange, DeckPart, DeckProtocol, Fade, Returned, Slot,
};

impl Deck {
    pub(super) fn apply(
        &mut self,
        part: DeckPart,
        at: SessionFrame,
        seq: Seq,
        context_valid: bool,
        read_pcm: ReadPcm,
        cut: Option<super::continuation::SourceCut>,
    ) -> Option<DeckPart> {
        self.interrupt_underrun(&part, at);
        let carried = cut.is_some_and(|cut| cut.carries(&part));
        match part {
            DeckPart::Attach {
                slot,
                item_id,
                pcm,
                segment,
            } => {
                let track = self.source_track(slot, pcm, segment, item_id, seq, cut);
                self.attach(slot, item_id, segment, at, seq, track)
            }
            DeckPart::Detach { slot } => self.detach(slot, context_valid, read_pcm),
            DeckPart::Start { slot, fade } => {
                if !carried && let Some(track) = self.tracks.at_mut(slot) {
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
                let track = self.source_track(slot, pcm, segment, item_id, seq, cut);
                Some(if carried {
                    self.continue_track(slot, track)
                } else {
                    self.replace_track(slot, track, context_valid, read_pcm)
                })
            }
            DeckPart::Chain { from, to } => Some(self.chain_tracks(from, to)),
            DeckPart::ContinueSource { to, .. } => cut.map(|cut| {
                DeckPart::Returned(Returned::SourceContinued {
                    to,
                    mark: cut.point.mark(at),
                    media_origin: cut.point.origin,
                })
            }),
            DeckPart::Clock { slot, clock } => {
                if let Some(track) = self.tracks.at_mut(slot) {
                    track.set_clock(clock);
                }
                Some(DeckPart::Clock { slot, clock })
            }
            DeckPart::Mix(change) => {
                self.mix.apply_change(change);
                if let DeckMixSettingsChange::Filter(knob) = change {
                    self.render.set_filter_knob(knob);
                } else {
                    self.render.set_mix(self.mix);
                }
                Some(DeckPart::Mix(change))
            }
            DeckPart::Eq(DeckEqChange::Gain { band, gain }) => {
                self.render.set_eq_gain(band, gain);
                Some(DeckPart::Eq(DeckEqChange::Gain { band, gain }))
            }
            DeckPart::Eq(DeckEqChange::Layout(layout)) => {
                self.eq_programs.replace_layout(seq);
                self.render
                    .take_eq_layout(layout)
                    .map(|layout| DeckPart::Returned(Returned::Eq(layout)))
            }
            DeckPart::EqProgram {
                binding,
                authority,
                program,
            } => {
                self.eq_programs.begin(program.band(), seq);
                Some(DeckPart::EqProgram {
                    binding,
                    authority,
                    program,
                })
            }
            DeckPart::Fx(change) => self.render.change_fx(change, self.sample_rate),
            DeckPart::FilterProgram {
                binding,
                authority,
                program,
            } => {
                self.begin_filter_program(seq);
                Some(DeckPart::FilterProgram {
                    binding,
                    authority,
                    program,
                })
            }
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
        segment: SegmentId,
        at: SessionFrame,
        seq: Seq,
        track: PlayerTrack,
    ) -> Option<DeckPart> {
        #[cfg(not(feature = "usdt"))]
        let _ = (item_id, segment, at, seq);
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

    /// Seats `track` stopped, as an attachment does; only a start plays it.
    fn replace_track(
        &mut self,
        slot: Slot,
        track: PlayerTrack,
        context_valid: bool,
        read_pcm: ReadPcm,
    ) -> DeckPart {
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

    /// A same-slot continuation hands the slot's envelope to the aligned
    /// receiver, so the cut sounds one signal instead of an eviction tail
    /// over it.
    fn continue_track(&mut self, slot: Slot, mut track: PlayerTrack) -> DeckPart {
        let Some(current) = self.tracks.at_mut(slot) else {
            return returned_pcm(slot, track);
        };
        track.continue_slot(current);
        returned_pcm(slot, std::mem::replace(current, track))
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
