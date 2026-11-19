use kithara_command::{Due, LevelInbox, Target};
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_signal::{SegmentId, SessionFrame};

use super::{
    super::{
        processor::Deck,
        track::{CutPlan, CutPoint, CutPrefix, PlayerResource, PlayerTrack, same_motion, scan_cut},
    },
    state::EndAction,
};
use crate::{
    PcmPacket, PcmReceiver,
    bridge::{DeckPart, DeckProtocol, DeckRefusal, FadeDir, Slot},
    rt::TrackSlots,
};

#[derive(Clone, Copy)]
pub(super) struct SourceCut {
    pub(super) point: CutPoint,
    private: Option<Slot>,
}

pub(super) enum Preparation {
    Ready(Option<SourceCut>),
    Pending {
        from: Slot,
        prefix: Option<CutPrefix>,
    },
}

struct Continuation {
    from: Slot,
    to: Slot,
    window: u64,
}

impl Continuation {
    fn admit(
        due: &Due<'_, DeckProtocol>,
        context_valid: bool,
    ) -> Result<Option<Self>, DeckRefusal> {
        let Some(DeckPart::ContinueSource { from, to, window }) = due.commands().first() else {
            if due
                .commands()
                .iter()
                .any(|part| matches!(part, DeckPart::ContinueSource { .. }))
            {
                return Err(DeckRefusal::Deferral);
            }
            return Ok(None);
        };
        if !context_valid {
            return Err(DeckRefusal::AxisRestarted);
        }
        let (from, to) = (*from, *to);
        let refusal = DeckRefusal::SourceContinuation { slot: to };
        if ![from, to]
            .into_iter()
            .all(|slot| due.basis().iter().any(|(named, _)| *named == slot))
        {
            return Err(refusal);
        }
        let window = u64::try_from(window.get())
            .ok()
            .filter(|frames| *frames > 0)
            .ok_or(refusal)?;
        Ok(Some(Self { from, to, window }))
    }

    fn plan<'a, I: Iterator<Item = (&'a PcmPacket, usize)>>(
        &self,
        old: &'a PlayerTrack,
        packets: impl Fn() -> I,
        segment: SegmentId,
        logical: Duration,
        prefix: Option<CutPrefix>,
        limit: usize,
    ) -> Result<CutPlan, DeckRefusal> {
        let mut plan = scan_cut(packets(), segment, logical, prefix, limit)
            .map_err(|()| DeckRefusal::SourceRange { slot: self.to })?;
        if let Some(point) = plan.point
            && !same_motion(
                old.packets(),
                from_cut(packets(), plan.retire_packets, point),
                old.segment(),
                segment,
                self.window,
                crate::consts::CAPACITY.get(),
            )
            .map_err(|()| DeckRefusal::SourceMotion { slot: self.to })?
        {
            plan.point = None;
        }
        Ok(plan)
    }

    fn prepare_private(
        &self,
        old: &PlayerTrack,
        pcm: &mut PcmReceiver,
        segment: SegmentId,
        logical: Duration,
        prefix: Option<CutPrefix>,
        budget: &mut usize,
    ) -> Result<CutPlan, DeckRefusal> {
        let plan = self.plan(
            old,
            || pcm.packets().map(|packet| (packet, 0)),
            segment,
            logical,
            prefix,
            (*budget).min(crate::consts::CAPACITY.get()),
        )?;
        if !retire(pcm, &plan, budget) || plan.point.is_none() && pcm.is_closed() {
            return Err(self.refusal());
        }
        Ok(plan)
    }

    fn prepare_staged(
        &self,
        tracks: &mut TrackSlots,
        item: TrackId,
        logical: Duration,
        prefix: Option<CutPrefix>,
        budget: &mut usize,
    ) -> Result<CutPlan, DeckRefusal> {
        let refusal = self.refusal();
        let staged = tracks
            .at_mut(self.to)
            .filter(|track| track.stages_item(item))
            .ok_or(refusal)?;
        staged.recycle_obsolete(budget);
        let old = tracks.at(self.from).ok_or(refusal)?;
        let staged = tracks.at(self.to).ok_or(refusal)?;
        let plan = self.plan(
            old,
            || staged.packets(),
            staged.segment(),
            logical,
            prefix,
            (*budget).min(crate::consts::CAPACITY.get()),
        )?;
        let staged = tracks.at_mut(self.to).ok_or(refusal)?;
        let count = plan.retire_frames + plan.point.map_or(0, |point| point.advance);
        if !staged.trim_cut(count, budget) || plan.point.is_none() && staged.cut_closed() {
            return Err(refusal);
        }
        *budget = budget.saturating_sub(plan.retire_packets);
        Ok(plan)
    }

    fn refusal(&self) -> DeckRefusal {
        DeckRefusal::SourceContinuation { slot: self.to }
    }
}

impl Deck {
    pub(super) fn prepare_continuation(
        &mut self,
        due: &mut Due<'_, DeckProtocol>,
        context_valid: bool,
        prefix: Option<CutPrefix>,
    ) -> Result<Preparation, DeckRefusal> {
        let Some(entry) = Continuation::admit(due, context_valid)? else {
            return Ok(Preparation::Ready(None));
        };
        let Continuation { from, to, .. } = entry;
        let refusal = entry.refusal();
        if self.armed.get(from.index()).ok_or(refusal)?.is_some() {
            return Err(DeckRefusal::Occupied { slot: from });
        }
        let old = self.tracks.at(from).ok_or(refusal)?;
        let (item, logical) = old.sounding_source(due.at()).ok_or(refusal)?;
        let budget = self.recycle.get_mut(to.index()).ok_or(refusal)?;
        let (plan, private) = match receiver(due.commands_mut(), from, to, item).ok_or(refusal)? {
            Some((pcm, segment)) => (
                entry.prepare_private(old, pcm, segment, logical, prefix, budget)?,
                Some(to),
            ),
            None => (
                entry.prepare_staged(&mut self.tracks, item, logical, prefix, budget)?,
                None,
            ),
        };
        Ok(plan.point.map_or(
            Preparation::Pending {
                from,
                prefix: plan.prefix,
            },
            |point| Preparation::Ready(Some(SourceCut { point, private })),
        ))
    }

    pub(in crate::rt) fn resume_continuations(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        at: SessionFrame,
        context_valid: bool,
    ) {
        self.release_armed(level);
        for index in 0..self.armed.len() {
            let Some(armed) = self.armed[index] else {
                continue;
            };
            let EndAction::ContinueSource {
                checked,
                epoch,
                prefix,
            } = armed.action
            else {
                continue;
            };
            if checked == at {
                continue;
            }
            self.armed[index] = None;
            if let Some(due) = level.resume(armed.seq, at, at) {
                self.take_due_with_prefix(
                    due,
                    context_valid && self.epoch == epoch,
                    PlayerResource::read,
                    prefix,
                );
            }
        }
    }

    pub(super) fn source_track(
        &self,
        slot: Slot,
        pcm: Box<PcmReceiver>,
        segment: SegmentId,
        item: TrackId,
        seq: kithara_command::Seq,
        cut: Option<SourceCut>,
    ) -> PlayerTrack {
        let mut track = self.track(pcm, segment, item, seq);
        if let Some(cut) = cut.filter(|cut| cut.private == Some(slot)) {
            // Preflight left exactly this packet at the private receiver head.
            let aligned = track.trim_cut(cut.point.offset, &mut 0);
            debug_assert!(aligned, "the validated private head cannot change");
            track.recycle_obsolete(&mut 0);
        }
        track
    }
}

fn receiver(
    parts: &mut [DeckPart],
    from: Slot,
    to: Slot,
    item: TrackId,
) -> Option<Option<(&mut PcmReceiver, SegmentId)>> {
    let mut incoming = None;
    let mut starts = 0;
    let mut exits = 0;
    let mut can_seat = true;
    for part in &mut parts[1..] {
        match part {
            DeckPart::Fade {
                slot,
                dir: FadeDir::In,
                ..
            }
            | DeckPart::Start { slot, .. }
                if *slot == to =>
            {
                starts += 1;
                can_seat = false;
            }
            DeckPart::Fade {
                slot,
                dir: FadeDir::Out,
                ..
            } if from != to && *slot == from => exits += 1,
            DeckPart::Clock { slot, .. } if *slot == to => can_seat = false,
            part if can_seat => {
                incoming = Some(private_receiver(part, from, to, item)?);
                can_seat = false;
            }
            _ => return None,
        }
    }
    (starts == 1 && exits == usize::from(from != to) && (from != to || incoming.is_some()))
        .then_some(incoming)
}

fn private_receiver(
    part: &mut DeckPart,
    from: Slot,
    to: Slot,
    item: TrackId,
) -> Option<(&mut PcmReceiver, SegmentId)> {
    let ((
        false,
        DeckPart::Attach {
            slot,
            item_id,
            pcm,
            segment,
        },
    )
    | (
        true,
        DeckPart::Replace {
            slot,
            item_id,
            pcm,
            segment,
        },
    )) = (from == to, part)
    else {
        return None;
    };
    (*slot == to && *item_id == item).then_some((pcm.as_mut(), *segment))
}

fn retire(pcm: &mut PcmReceiver, plan: &CutPlan, budget: &mut usize) -> bool {
    for _ in 0..plan.retire_packets {
        if !pcm.recycle_head() {
            return false;
        }
        *budget -= 1;
    }
    true
}

fn from_cut<'a>(
    packets: impl Iterator<Item = (&'a PcmPacket, usize)>,
    retired: usize,
    point: CutPoint,
) -> impl Iterator<Item = (&'a PcmPacket, usize)> {
    packets
        .skip(retired)
        .enumerate()
        .map(move |(index, (packet, offset))| {
            (packet, if index == 0 { point.offset } else { offset })
        })
}
