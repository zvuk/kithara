use std::{num::NonZeroU32, ops::Deref, task::Waker};

use kithara::{
    bufpool::HasPool,
    events::TrackId,
    host::{DeckControl, DeckId, HostCore},
    link::{
        GridAnswer, LinkedFactory, LinkedHost, LinkedHostDeck, LinkedPlayer, LinkedSnapshot,
        TempoTrajectory,
    },
    platform::{
        sync::{Arc, Mutex},
        tokio::sync::oneshot,
    },
    play::{
        Bound, DeckPass, HostedDeck, Outbox, PlayError, PlayWorker, Player, PlayerConfig,
        PlayerFactory, PlayerImpl, Position, ResourcePrep, Settled, Track, TrackCommand,
        TrackFactory, TrackReceipt, TrackSettings, TrackSettingsChange, TrackSnapshot,
    },
    queue::{Queue, QueueCommand, QueueControl, TrackSource, Transition},
    signal::{FrameCount, SegmentId, SessionFrame},
};
use kithara_command::{Outcome, Rejection, Seq, When};
use kithara_render::{
    LoadRefusal,
    bridge::{DeckEvent, DeckPart, SlotSnapshot, SlotState},
    rt::DeckMixerConfig,
};

use super::OfflineHostHarness;

pub type LinkedOwner<S> = LinkedHost<S, HostCore<S, LinkedHostDeck<S>>>;
pub type LinkedQueue<S> = Queue<S, LinkedFactory<ObservedFactory<S>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkVerdict {
    Applied,
    Capacity,
    Stale,
    Cancelled,
    Refused,
}

pub enum LinkProbeCommand<S: HasPool<u8> + Send + Sync + 'static> {
    ReadSnapshot {
        answer: oneshot::Sender<Vec<(LinkedSnapshot<TrackSnapshot>, SlotSnapshot)>>,
    },
    Track {
        item: TrackId,
        command: TrackCommand<S>,
    },
    Incoming {
        item: TrackId,
        source: TrackSource<S>,
        cue: Option<Position>,
    },
}

struct LinkProbes<S: HasPool<u8> + Send + Sync + 'static> {
    commands: Vec<LinkProbeCommand<S>>,
    wake: Option<Waker>,
}

#[derive(Default)]
pub struct LinkObservation {
    pub capture: Option<LinkCapture>,
    pub tracks: Vec<LinkedSnapshot<TrackSnapshot>>,
    pub consumed: Vec<(TrackId, SlotSnapshot)>,
    pub consumed_rate: Option<NonZeroU32>,
    pub placements: Vec<LinkPlacement>,
    pub speeds: Vec<LinkSpeed>,
    pub cue_curves: Vec<LinkCue>,
    pub owner_inputs: Vec<LinkOwnerInput>,
    pub placement_applied: Vec<(TrackId, Seq, bool)>,
    pub placement_receipts: Vec<(TrackId, LinkReceiptOrigin, Settled)>,
    pub sync_requests: Vec<LinkSyncRequest>,
    pub settled: Vec<(TrackId, Settled)>,
    pub loads: Vec<(TrackId, Seq)>,
    pub cues: Vec<(TrackId, Seq)>,
    pub starts: Vec<(TrackId, Seq)>,
    pub commands: Vec<(TrackId, Seq)>,
    pub seeks: Vec<(TrackId, LinkReceiptOrigin, Seq)>,
    pub seek_applied: Vec<(TrackId, Seq, bool)>,
    pub source_clocks: Vec<(TrackId, kithara::signal::AudioChunkInfo)>,
    pub dispatcher: Vec<(Seq, LinkVerdict)>,
    pub deck: Vec<(Seq, LinkVerdict)>,
    pub ready_cues: Vec<(TrackId, Seq)>,
    pub timeline: Vec<serde_json::Value>,
}

impl LinkObservation {
    pub fn timeline_evidence(&self) -> impl Iterator<Item = serde_json::Value> + '_ {
        self.timeline.iter().map(|entry| {
            let mut entry = entry.clone();
            for operation in ["SetSpeed", "Align"] {
                if let Some(input) = entry["details"][operation]["speed"]["input"].as_u64() {
                    let speed = self
                        .speeds
                        .get(usize::try_from(input).expect("speed input index"))
                        .expect("an admitted command retains its exact curve");
                    assert_eq!(entry["item"].as_u64(), Some(u64::from(speed.item)));
                    entry["details"][operation]["speed"] =
                        serde_json::Value::String(format!("{:?}", speed.curve));
                }
            }
            entry
        })
    }
}

#[derive(Default)]
pub struct LinkCapture {
    pub consumed: Vec<(
        SessionFrame,
        TrackId,
        kithara_render::bridge::Slot,
        SlotSnapshot,
    )>,
    pub starts: Vec<(
        TrackId,
        kithara_render::bridge::Slot,
        SessionFrame,
        kithara_render::bridge::Fade,
    )>,
    pub fades: Vec<(
        TrackId,
        kithara_render::bridge::Slot,
        SessionFrame,
        kithara_render::CrossfadeSettings,
        kithara_render::bridge::FadeDir,
    )>,
    pub segments: Vec<(
        TrackId,
        kithara_render::bridge::Slot,
        SessionFrame,
        u64,
        u64,
    )>,
    pub faded: Vec<(TrackId, kithara_render::bridge::Slot, SessionFrame)>,
    pub ended: Vec<(TrackId, kithara_render::bridge::Slot, SessionFrame)>,
    pub faults: Vec<DeckEvent>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkReceiptOrigin {
    Deck,
    Lane,
}

#[derive(Debug)]
pub struct LinkPlacement {
    pub item: TrackId,
    pub seq: Seq,
    pub aligned: bool,
    pub to: Position,
    pub at: SessionFrame,
    pub now: SessionFrame,
    pub delivery: FrameCount,
    pub before: TrackSnapshot,
    pub consumed: SlotSnapshot,
    pub planned: Result<(Position, f64), PlayError>,
    pub input: Option<usize>,
}

#[derive(Debug)]
pub struct LinkSyncRequest {
    pub on: bool,
    pub seq: Option<Seq>,
    pub input: usize,
    pub tracks: Vec<LinkedSnapshot<TrackSnapshot>>,
    pub placements: std::ops::Range<usize>,
}

#[derive(Debug)]
pub struct LinkSpeed {
    pub item: TrackId,
    pub seq: Seq,
    pub curve: kithara::sync::SpeedCurve,
    pub at: When<SessionFrame>,
    pub landing: Option<(Position, SessionFrame)>,
    pub before: TrackSnapshot,
    pub consumed: Option<SlotSnapshot>,
    pub sample_rate: Option<NonZeroU32>,
    pub input: Option<usize>,
    pub host_input: Option<usize>,
    pub planned: Option<Result<(Position, f64), PlayError>>,
}

#[derive(Debug)]
pub struct LinkOwnerInput {
    pub item: Option<TrackId>,
    pub at: Option<SessionFrame>,
    pub trajectory: Option<TempoTrajectory>,
    pub kind: LinkOwnerInputKind,
}

#[derive(Clone, Debug)]
pub enum LinkOwnerInputKind {
    Retime,
    Realign,
    Sync { on: bool },
    Grid { answer: Box<GridAnswer> },
}

#[derive(Debug)]
pub struct LinkCue {
    pub item: TrackId,
    pub seq: Seq,
    pub position: Position,
    pub curve: kithara::sync::SpeedCurve,
    pub before: TrackSnapshot,
    pub consumed: Option<SlotSnapshot>,
    pub host_input: Option<usize>,
    pub native: Option<LinkReceiptOrigin>,
    pub segment: Option<SegmentId>,
}

pub struct ObservedFactory<S> {
    observation: Arc<Mutex<LinkObservation>>,
    schema: std::marker::PhantomData<fn() -> S>,
}

impl<S> ObservedFactory<S> {
    pub fn new(observation: Arc<Mutex<LinkObservation>>) -> Self {
        Self {
            observation,
            schema: std::marker::PhantomData,
        }
    }
}

pub struct ObservedTrack<S> {
    inner: PlayerImpl<S>,
    observation: Arc<Mutex<LinkObservation>>,
}

impl<S> TrackFactory<S> for ObservedFactory<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Track = ObservedTrack<S>;

    fn track(&self, config: PlayerConfig) -> Result<Self::Track, PlayError> {
        Ok(ObservedTrack {
            inner: PlayerFactory.track(config)?,
            observation: self.observation.clone(),
        })
    }
}

fn speed_trace_evidence(
    result: &Result<Option<Seq>, PlayError>,
    curve: &kithara::sync::SpeedCurve,
    input: usize,
) -> serde_json::Value {
    if matches!(result, Ok(Some(_))) {
        serde_json::json!({"input": input})
    } else {
        serde_json::Value::String(format!("{curve:?}"))
    }
}

impl<S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static> ObservedTrack<S> {
    fn trace_entry(
        &self,
        event: &str,
        details: &serde_json::Value,
        out: &Outbox<'_, S>,
    ) -> serde_json::Value {
        let snapshot = self.inner.snapshot();
        let pass = out.pass();
        let slot = pass.and_then(|pass| {
            snapshot
                .slot
                .and_then(|slot| pass.deck.slots.get(usize::from(slot.get())).copied())
        });
        serde_json::json!({
            "event": event,
            "details": details,
            "item": u64::from(snapshot.item),
            "pass": pass.map(|pass| serde_json::json!({"now": i64::from(pass.now), "delivery": pass.delivery.get(), "sample_rate": pass.deck.sample_rate})),
            "slot": slot.map(|slot| format!("{slot:?}")),
            "track": format!("{snapshot:?}"),
        })
    }

    fn trace(&self, event: &str, details: &serde_json::Value, out: &Outbox<'_, S>) {
        let entry = self.trace_entry(event, details, out);
        self.observation.lock().timeline.push(entry);
    }

    fn trace_command(
        &self,
        command: &TrackCommand<S>,
        out: &Outbox<'_, S>,
    ) -> Option<serde_json::Value> {
        let details = match command {
            TrackCommand::Load { position, .. } => {
                Some(serde_json::json!({"Load": {"position_ns": position.as_nanos().to_string()}}))
            }
            TrackCommand::Seat { slot, at } => {
                Some(serde_json::json!({"Seat": {"slot": slot.get(), "at": format!("{at:?}")}}))
            }
            TrackCommand::Fade { at, settings, dir } => Some(
                serde_json::json!({"Fade": {"at": format!("{at:?}"), "settings": format!("{settings:?}"), "dir": format!("{dir:?}")}}),
            ),
            TrackCommand::Play { at } => Some(serde_json::json!({"Play": format!("{at:?}")})),
            TrackCommand::Pause { at } => Some(serde_json::json!({"Pause": format!("{at:?}")})),
            TrackCommand::Seek { to } => {
                Some(serde_json::json!({"Seek": to.as_nanos().to_string()}))
            }
            TrackCommand::Jump { to, at } => Some(
                serde_json::json!({"Jump": {"to_ns": to.as_nanos().to_string(), "at": i64::from(*at)}}),
            ),
            TrackCommand::Align { to, at, .. } => Some(
                serde_json::json!({"Align": {"to_ns": to.as_nanos().to_string(), "speed": null, "at": i64::from(*at)}}),
            ),
            TrackCommand::SetSpeed { at, .. } => {
                Some(serde_json::json!({"SetSpeed": {"speed": null, "at": format!("{at:?}")}}))
            }
            _ => None,
        };
        details.map(|details| self.trace_entry("command", &details, out))
    }

    fn native_seek(&self, command: &TrackCommand<S>) -> Option<(Position, TrackSnapshot)> {
        match command {
            TrackCommand::Seek { to }
                if !matches!(
                    self.inner.snapshot().status,
                    kithara::play::TrackStatus::Playing { .. }
                ) =>
            {
                Some((*to, self.inner.snapshot()))
            }
            _ => None,
        }
    }

    fn record_native_seek(
        &self,
        seq: Option<Seq>,
        native: Option<(Position, TrackSnapshot)>,
        origin: Option<LinkReceiptOrigin>,
    ) {
        if let Some(seq) = seq
            && let Some((position, before)) = native
            && let Some(curve) = self.inner.prepared_speed().cloned()
        {
            let mut observation = self.observation.lock();
            let host_input = observation
                .owner_inputs
                .iter()
                .rposition(|input| input.trajectory.is_some());
            observation.cue_curves.push(LinkCue {
                item: before.item,
                seq,
                position,
                curve,
                before,
                consumed: None,
                host_input,
                native: origin,
                segment: None,
            });
        }
    }

    fn record(&self, settled: &Settled, origin: Option<LinkReceiptOrigin>) {
        if !matches!(settled, Settled::Pending) {
            let item = self.inner.snapshot().item;
            let mut observation = self.observation.lock();
            observation.settled.push((item, settled.clone()));
            if let Some(origin) = origin {
                observation
                    .placement_receipts
                    .push((item, origin, settled.clone()));
            }
        }
    }
}

impl<S> Player<S> for ObservedTrack<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Command = TrackCommand<S>;
    type Snapshot = TrackSnapshot;

    delegate::delegate! {
        to self.inner {
            fn snapshot(&self) -> Self::Snapshot;
        }
    }

    fn entry(&self, bound: Bound) -> Option<SessionFrame> {
        let result = self.inner.entry(bound);
        let snapshot = self.inner.snapshot();
        self.observation.lock().timeline.push(serde_json::json!({
            "event": "entry",
            "item": u64::from(snapshot.item),
            "bound": format!("{bound:?}"),
            "frame": result.map(i64::from),
            "track": format!("{snapshot:?}"),
        }));
        result
    }

    fn tick(&mut self, now: SessionFrame, out: &mut Outbox<'_, S>) {
        self.inner.tick(now, out);
        let item = self.inner.snapshot().item;
        let pending_seeks: Vec<_> = {
            let observation = self.observation.lock();
            observation
                .seeks
                .iter()
                .filter_map(|&(owner, origin, seq)| {
                    (owner == item
                        && origin == LinkReceiptOrigin::Lane
                        && !observation
                            .seek_applied
                            .iter()
                            .any(|&(owner, answered, _)| owner == item && answered == seq))
                    .then_some(seq)
                })
                .collect()
        };
        for seq in pending_seeks {
            if let Some(applied) = self.inner.speed_applied(seq) {
                self.observation
                    .lock()
                    .seek_applied
                    .push((item, seq, applied));
            }
        }
        let pending_placements = {
            let observation = self.observation.lock();
            observation
                .placements
                .iter()
                .filter_map(|placement| {
                    (placement.item == item
                        && !observation
                            .placement_applied
                            .iter()
                            .any(|(owner, seq, _)| *owner == item && *seq == placement.seq))
                    .then_some(placement.seq)
                })
                .collect::<Vec<_>>()
        };
        for seq in pending_placements {
            if let Some(applied) = self.inner.speed_applied(seq) {
                self.observation
                    .lock()
                    .placement_applied
                    .push((item, seq, applied));
            }
        }
        let pending: Vec<_> = self
            .observation
            .lock()
            .cues
            .iter()
            .filter_map(|(owner, seq)| (*owner == item).then_some(*seq))
            .collect();
        for seq in pending {
            if self.inner.speed_applied(seq) == Some(true) {
                let mut observation = self.observation.lock();
                if !observation.ready_cues.contains(&(item, seq)) {
                    observation.ready_cues.push((item, seq));
                }
            }
        }
        let native: Vec<_> = self
            .observation
            .lock()
            .cue_curves
            .iter()
            .filter_map(|cue| {
                (cue.item == item && cue.native.is_some()).then_some((cue.seq, cue.native))
            })
            .collect();
        for (seq, origin) in native {
            let ready = if origin == Some(LinkReceiptOrigin::Lane) {
                self.inner.speed_applied(seq) == Some(true)
            } else {
                self.observation.lock().placement_receipts.iter().any(|(owner, origin, receipt)| *owner == item
                    && *origin == LinkReceiptOrigin::Deck && matches!(receipt, Settled::Applied { seq: answered, .. } if *answered == seq))
            };
            if ready {
                let mut observation = self.observation.lock();
                if !observation.ready_cues.contains(&(item, seq)) {
                    observation.ready_cues.push((item, seq));
                }
            }
        }
        let snapshot = self.inner.snapshot();
        if let Some(mark) = snapshot.mark
            && let Some(pass) = out.pass()
            && let Some(slot) = snapshot
                .slot
                .and_then(|slot| pass.deck.slots.get(usize::from(slot.get())))
            && slot.mark == Some(mark)
            && slot.state == SlotState::Playing
        {
            let mut observation = self.observation.lock();
            if !observation.cue_curves.iter().any(|cue| {
                cue.item == item
                    && cue
                        .consumed
                        .and_then(|slot| slot.mark)
                        .is_some_and(|first| first.lane.segment == mark.lane.segment)
            }) && let Some(index) = observation.cue_curves.iter().rposition(|cue| {
                cue.item == item
                    && cue.consumed.is_none()
                    && observation.ready_cues.contains(&(item, cue.seq))
                    && cue
                        .before
                        .mark
                        .is_none_or(|before| before.lane.segment != mark.lane.segment)
                    && cue
                        .segment
                        .is_none_or(|segment| segment == mark.lane.segment)
                    && (cue.native != Some(LinkReceiptOrigin::Deck) || cue.segment.is_some())
            }) {
                observation.cue_curves[index].consumed = Some(*slot);
            }
        }
    }

    fn apply(
        &mut self,
        command: Self::Command,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let traced = self.trace_command(&command, out);
        let loading = matches!(command, TrackCommand::Load { .. });
        let native = self.native_seek(&command);
        let seek_origin = matches!(command, TrackCommand::Seek { .. }).then(|| {
            if self.inner.snapshot().attached {
                LinkReceiptOrigin::Deck
            } else {
                LinkReceiptOrigin::Lane
            }
        });
        let aligned = matches!(command, TrackCommand::Align { .. });
        let speed = match &command {
            TrackCommand::SetSpeed { speed, at } => Some((speed.clone(), *at, None)),
            TrackCommand::Align { to, speed, at } => Some((
                speed.clone(),
                When::At(*at - self.inner.snapshot().declick),
                Some((*to, *at)),
            )),
            _ => None,
        }
        .map(|(curve, at, landing)| {
            let before = self.inner.snapshot();
            let pass = out.pass();
            let consumed = pass.and_then(|pass| {
                before
                    .slot
                    .and_then(|slot| pass.deck.slots.get(usize::from(slot.get())).copied())
            });
            let sample_rate = pass.and_then(|pass| NonZeroU32::new(pass.deck.sample_rate));
            let planned = match (at, sample_rate) {
                (When::At(frame), Some(rate)) => Some(self.inner.planned(frame, rate)),
                _ => None,
            };
            (curve, at, landing, before, consumed, sample_rate, planned)
        });
        let placement = out.pass().and_then(|pass| {
            let before = self.inner.snapshot();
            let consumed = before
                .slot
                .and_then(|slot| pass.deck.slots.get(usize::from(slot.get())).copied())?;
            match &command {
                TrackCommand::Jump { to, at } | TrackCommand::Align { to, at, .. } => {
                    let rate = NonZeroU32::new(pass.deck.sample_rate)?;
                    let planned = self.inner.planned(*at, rate);
                    Some((
                        (*to, planned),
                        *at,
                        pass.now,
                        pass.delivery,
                        before,
                        consumed,
                    ))
                }
                _ => None,
            }
        });
        let result = self.inner.apply(command, out);
        if let Some(mut entry) = traced {
            let admission =
                self.trace_entry("admission", &serde_json::json!(format!("{result:?}")), out);
            let mut observation = self.observation.lock();
            if let Some((curve, ..)) = &speed {
                let operation = if aligned { "Align" } else { "SetSpeed" };
                entry["details"][operation]["speed"] =
                    speed_trace_evidence(&result, curve, observation.speeds.len());
            }
            if let (
                Ok(Some(seq)),
                Some((curve, at, landing, before, consumed, sample_rate, planned)),
            ) = (&result, speed)
            {
                let input = observation
                    .owner_inputs
                    .iter()
                    .rposition(|input| input.item.is_none_or(|item| item == before.item));
                let host_input = observation
                    .owner_inputs
                    .iter()
                    .rposition(|input| input.trajectory.is_some());
                observation.speeds.push(LinkSpeed {
                    item: before.item,
                    seq: *seq,
                    curve,
                    at,
                    landing,
                    before,
                    consumed,
                    sample_rate,
                    input,
                    host_input,
                    planned,
                });
            }
            observation.timeline.push(entry);
            observation.timeline.push(admission);
        }
        let seq = result?;
        self.record_native_seek(seq, native, seek_origin);
        if let Some(seq) = seq
            && let Some(origin) = seek_origin
        {
            self.observation
                .lock()
                .seeks
                .push((self.inner.snapshot().item, origin, seq));
        }
        if let Some(seq) = seq
            && let Some(((to, planned), at, now, delivery, before, consumed)) = placement
        {
            let item = before.item;
            let input = self
                .observation
                .lock()
                .owner_inputs
                .iter()
                .rposition(|input| input.item.is_none_or(|owner| owner == item));
            let placement = LinkPlacement {
                item,
                seq,
                aligned,
                to,
                at,
                now,
                delivery,
                before,
                consumed,
                planned,
                input,
            };
            self.observation.lock().placements.push(placement);
        }
        if loading && let Some(seq) = seq {
            self.observation
                .lock()
                .loads
                .push((self.inner.snapshot().item, seq));
        }
        Ok(seq)
    }

    fn settle(&mut self, receipt: TrackReceipt<'_, S>, out: &mut Outbox<'_, S>) -> Settled {
        if let TrackReceipt::Event(event) = &receipt {
            let diagnostic = match event {
                DeckEvent::UnderrunStarted { slot, item, at, .. } => Some(serde_json::json!({
                    "UnderrunStarted": {"slot": slot.get(), "item": item.as_u64(), "at": i64::from(*at)},
                })),
                DeckEvent::UnderrunEnded {
                    slot,
                    item,
                    at,
                    frames,
                    recovered: true,
                    ..
                } => Some(serde_json::json!({
                    "Underrun": {"slot": slot.get(), "item": item.as_u64(), "at": i64::from(*at), "frames": frames},
                })),
                DeckEvent::UnderrunEnded {
                    slot,
                    item,
                    at,
                    frames,
                    recovered: false,
                    ..
                } => Some(serde_json::json!({
                    "UnderrunEnded": {"slot": slot.get(), "item": item.as_u64(), "at": i64::from(*at), "frames": frames},
                })),
                _ => None,
            };
            if let Some(diagnostic) = diagnostic {
                self.trace("deck event", &diagnostic, out);
            }
        }
        if let TrackReceipt::Loaded(receipt) = &receipt {
            self.trace(
                "load receipt",
                &serde_json::json!({"seq": receipt.seq().get(), "outcome": format!("{:?}", receipt.outcome())}),
                out,
            );
        }
        if let TrackReceipt::Loaded(receipt) = &receipt
            && let Outcome::Applied {
                data: kithara_render::Dispatched::Loaded(loaded),
                ..
            } = receipt.outcome()
            && let Some(clock) = loaded.opened.pcm.next_chunk_info()
        {
            self.observation
                .lock()
                .source_clocks
                .push((self.inner.snapshot().item, clock));
        }
        let origin =
            matches!(receipt, TrackReceipt::Deck { .. }).then_some(LinkReceiptOrigin::Deck);
        if let TrackReceipt::Deck {
            seq,
            outcome: Outcome::Applied { .. },
            batch,
        } = &receipt
        {
            let snapshot = self.inner.snapshot();
            let segment = batch.commands.iter().find_map(|part| match part {
                DeckPart::Adopt { slot, segment } if Some(*slot) == snapshot.slot => Some(*segment),
                _ => None,
            });
            if let Some(segment) = segment {
                let mut observation = self.observation.lock();
                if let Some(cue) = observation.cue_curves.iter_mut().find(|cue| {
                    cue.item == snapshot.item
                        && cue.seq == *seq
                        && cue.native == Some(LinkReceiptOrigin::Deck)
                }) {
                    cue.segment = Some(segment);
                }
            }
            if batch.commands.iter().any(
                |part| matches!(part, DeckPart::Start { slot, .. } if Some(*slot) == snapshot.slot),
            ) {
                let mut observation = self.observation.lock();
                let start = (snapshot.item, *seq);
                if !observation.starts.contains(&start) {
                    observation.starts.push(start);
                }
            }
        }
        if let TrackReceipt::Deck {
            seq,
            outcome,
            batch,
        } = &receipt
        {
            let parts = batch
                .commands
                .iter()
                .filter_map(|part| match part {
                    DeckPart::Attach {
                        slot,
                        item_id,
                        segment,
                        ..
                    } => Some(
                        serde_json::json!({"Attach": {"slot": slot.get(), "item": item_id.as_u64(), "segment": segment.get()}}),
                    ),
                    DeckPart::Replace {
                        slot,
                        item_id,
                        segment,
                        ..
                    } => Some(
                        serde_json::json!({"Replace": {"slot": slot.get(), "item": item_id.as_u64(), "segment": segment.get()}}),
                    ),
                    DeckPart::Adopt { slot, segment } => Some(
                        serde_json::json!({"Adopt": {"slot": slot.get(), "segment": segment.get()}}),
                    ),
                    DeckPart::Start { slot, fade } => Some(
                        serde_json::json!({"Start": {"slot": slot.get(), "fade": format!("{fade:?}")}}),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let applied_frame = match outcome {
                Outcome::Applied { at, .. } => Some(i64::from(*at)),
                Outcome::Rejected(_) => None,
            };
            self.trace(
                "deck receipt",
                &serde_json::json!({"seq": seq.get(), "outcome": format!("{outcome:?}"), "applied_frame": applied_frame, "parts": parts}),
                out,
            );
        }
        let settled = self.inner.settle(receipt, out);
        self.record(&settled, origin);
        settled
    }
}

impl<S> Track<S> for ObservedTrack<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    delegate::delegate! {
        to self.inner {
            fn admit(&mut self, change: TrackSettingsChange, at: When<SessionFrame>, out: &Outbox<'_, S>) -> Result<Option<usize>, PlayError>;
            fn projected(&self) -> TrackSettings;
            fn prepared_speed(&self) -> Option<&kithara::sync::SpeedCurve>;
            fn prepare_entry(&mut self, at: SessionFrame, out: &mut Outbox<'_, S>) -> Result<bool, PlayError>;
            fn planned_end(&self, sample_rate: NonZeroU32) -> Result<Option<SessionFrame>, PlayError>;
            fn speed_applied(&mut self, seq: Seq) -> Option<bool>;
            fn finish_group(&mut self, result: Result<Seq, &mut Vec<DeckPart>>);
        }
    }

    fn planned(
        &self,
        at: SessionFrame,
        sample_rate: NonZeroU32,
    ) -> Result<(Position, f64), PlayError> {
        let result = self.inner.planned(at, sample_rate);
        self.observation.lock().timeline.push(serde_json::json!({
            "event": "planned",
            "item": u64::from(self.inner.snapshot().item),
            "at": i64::from(at),
            "sample_rate": sample_rate.get(),
            "result": format!("{result:?}"),
            "track": format!("{:?}", self.inner.snapshot()),
        }));
        result
    }

    fn cue(
        &mut self,
        position: Position,
        speed: kithara::sync::SpeedCurve,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let before = self.inner.snapshot();
        let curve = speed.clone();
        self.trace(
            "cue",
            &serde_json::json!({"position_ns": position.as_nanos().to_string(), "speed": format!("{speed:?}")}),
            out,
        );
        let seq = self.inner.cue(position, speed, out)?;
        if let Some(seq) = seq {
            let item = self.inner.snapshot().item;
            let mut observation = self.observation.lock();
            observation.cues.push((item, seq));
            let host_input = observation
                .owner_inputs
                .iter()
                .rposition(|input| input.trajectory.is_some());
            observation.cue_curves.push(LinkCue {
                item,
                seq,
                position,
                curve,
                before,
                consumed: None,
                host_input,
                native: None,
                segment: None,
            });
        }
        Ok(seq)
    }

    fn speed_receipt(&mut self) -> Option<Settled> {
        let receipt = self.inner.speed_receipt();
        if let Some(receipt) = &receipt {
            self.record(receipt, Some(LinkReceiptOrigin::Lane));
        }
        receipt
    }
}

pub struct LinkedQueueHandle<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    id: DeckId,
    control: QueueControl<S>,
    observation: Arc<Mutex<LinkObservation>>,
    commands: Arc<Mutex<LinkProbes<S>>>,
}

impl<S> LinkedQueueHandle<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub fn probe(&self, command: LinkProbeCommand<S>) {
        let wake = {
            let mut probes = self.commands.lock();
            probes.commands.push(command);
            probes.wake.clone()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    delegate::delegate! {
        to self {
            #[field(id)]
            pub const fn id(&self) -> DeckId;
            #[field(&control)]
            pub const fn control(&self) -> &QueueControl<S>;
        }
    }

    pub fn observe<Value>(&self, read: impl FnOnce(&LinkObservation) -> Value) -> Value {
        read(&self.observation.lock())
    }
    pub async fn read_snapshot(&self) -> Vec<(LinkedSnapshot<TrackSnapshot>, SlotSnapshot)> {
        let (answer, reply) = oneshot::channel();
        self.probe(LinkProbeCommand::ReadSnapshot { answer });
        reply
            .await
            .expect("Linked snapshot query reaches its owner")
    }

    pub fn track_snapshot(&self, item: TrackId) -> Option<LinkedSnapshot<TrackSnapshot>> {
        self.observe(|observation| {
            observation
                .tracks
                .iter()
                .find(|track| track.track.item == item)
                .cloned()
        })
    }
}

impl<S> Deref for LinkedQueueHandle<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Target = QueueControl<S>;
    fn deref(&self) -> &Self::Target {
        &self.control
    }
}

struct ObservedQueue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    inner: LinkedQueue<S>,
    observation: Arc<Mutex<LinkObservation>>,
    commands: Arc<Mutex<LinkProbes<S>>>,
}

impl<S> ObservedQueue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    fn publish(&self) {
        self.observation.lock().tracks = self.inner.tracks_active().map(Player::snapshot).collect();
    }

    fn publish_pass(&self, pass: DeckPass<'_>) {
        self.publish_observation(pass, true);
    }

    fn publish_consumed(&self, pass: DeckPass<'_>) {
        self.publish_observation(pass, false);
    }

    fn publish_observation(&self, pass: DeckPass<'_>, publish_tracks: bool) {
        let tracks: Vec<_> = self.inner.tracks_active().map(Player::snapshot).collect();
        let mut observation = self.observation.lock();
        observation.consumed_rate = NonZeroU32::new(pass.deck.sample_rate);
        observation.consumed = tracks
            .iter()
            .filter_map(|track| {
                let slot = track.track.slot?;
                pass.deck
                    .slots
                    .get(usize::from(slot.get()))
                    .copied()
                    .map(|snapshot| (track.track.item, snapshot))
            })
            .collect();
        if observation.capture.is_some() {
            let owners: Vec<_> = tracks
                .iter()
                .filter_map(|track| track.track.slot.map(|slot| (track.track.item, slot)))
                .collect();
            let LinkObservation {
                consumed, capture, ..
            } = &mut *observation;
            let capture = capture.as_mut().expect("opt-in typed capture");
            for (item, snapshot) in consumed.iter() {
                let slot = &owners
                    .iter()
                    .find(|(owner, _)| owner == item)
                    .expect("active owner")
                    .1;
                assert_eq!(
                    owners.iter().filter(|(_, other)| other == slot).count(),
                    1,
                    "a consumed slot must have one active Queue owner"
                );
                if let (Some(mark), Some(source)) = (snapshot.mark, snapshot.source) {
                    assert_eq!(
                        mark.lane.segment, source.segment,
                        "consumed segment custody"
                    );
                }
                let duplicate = capture
                    .consumed
                    .iter()
                    .rev()
                    .take_while(|(at, ..)| *at == pass.now)
                    .any(|(_, owner, seat, prior)| {
                        owner == item && seat == slot && prior == snapshot
                    });
                if !duplicate {
                    assert!(
                        capture.consumed.len() < 65_536,
                        "bounded queue-crossfade capture"
                    );
                    capture.consumed.push((pass.now, *item, *slot, *snapshot));
                }
            }
        }
        if publish_tracks {
            observation.tracks = tracks;
        }
    }

    fn capture_native(&self, receipt: &TrackReceipt<'_, S>) {
        if let TrackReceipt::Deck {
            seq,
            outcome: Outcome::Applied { at, .. },
            batch,
            ..
        } = receipt
        {
            let owners: Vec<_> = self
                .inner
                .tracks_active()
                .filter_map(|track| {
                    let track = track.snapshot().track;
                    track.slot.map(|slot| (slot, track.item))
                })
                .collect();
            if let Some(capture) = self.observation.lock().capture.as_mut() {
                for part in &batch.commands {
                    let slot = match part {
                        DeckPart::Adopt { slot, .. }
                        | DeckPart::Start { slot, .. }
                        | DeckPart::Fade { slot, .. } => *slot,
                        DeckPart::Replace { .. } => {
                            panic!("five-item automatic queue must not replace an occupied slot")
                        }
                        _ => continue,
                    };
                    let candidates: Vec<_> =
                        owners.iter().filter(|(seat, _)| *seat == slot).collect();
                    assert_eq!(candidates.len(), 1, "one native batch owner per slot");
                    let item = candidates[0].1;
                    match part {
                        DeckPart::Adopt { segment, .. } => {
                            let entry = (item, slot, *at, segment.get(), seq.get());
                            if !capture.segments.contains(&entry) {
                                assert!(
                                    capture.segments.len() < 16,
                                    "bounded native segment custody"
                                );
                                capture.segments.push(entry);
                            }
                        }
                        DeckPart::Start { fade, .. } => {
                            let entry = (item, slot, *at, *fade);
                            if !capture.starts.contains(&entry) {
                                assert!(capture.starts.len() < 5, "five automatic starts");
                                capture.starts.push(entry);
                            }
                        }
                        DeckPart::Fade { settings, dir, .. } => {
                            let entry = (item, slot, *at, *settings, *dir);
                            if !capture.fades.contains(&entry) {
                                assert!(capture.fades.len() < 8, "four queue fade pairs");
                                capture.fades.push(entry);
                            }
                        }
                        _ => unreachable!("matched capture part"),
                    }
                }
            }
        }
    }

    fn capture_terminal(&self, receipt: &TrackReceipt<'_, S>) {
        if let TrackReceipt::Event(event) = receipt {
            let boundary = match event {
                DeckEvent::Faded { slot, at } => Some((*slot, *at, true)),
                DeckEvent::Ended { slot, at } => Some((*slot, *at, false)),
                _ => None,
            };
            if let Some((slot, at, faded)) = boundary {
                let owners: Vec<_> = self
                    .inner
                    .tracks_active()
                    .filter_map(|track| {
                        let snapshot = track.snapshot().track;
                        (snapshot.slot == Some(slot)).then_some(snapshot.item)
                    })
                    .collect();
                if let Some(capture) = self.observation.lock().capture.as_mut() {
                    assert_eq!(owners.len(), 1, "a terminal slot retains one native owner");
                    let entries = if faded {
                        &mut capture.faded
                    } else {
                        &mut capture.ended
                    };
                    let entry = (owners[0], slot, at);
                    if !entries.contains(&entry) {
                        assert!(entries.len() < 5, "five native track boundaries");
                        entries.push(entry);
                    }
                }
            } else if matches!(
                event,
                DeckEvent::Failed { .. }
                    | DeckEvent::UnderrunStarted { .. }
                    | DeckEvent::UnderrunEnded { .. }
            ) && let Some(capture) = self.observation.lock().capture.as_mut()
            {
                assert!(capture.faults.len() < 16, "bounded queue fault evidence");
                capture.faults.push(*event);
            }
        }
    }
}

impl<S> DeckControl for ObservedQueue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Control = QueueControl<S>;

    fn control(&self) -> Self::Control {
        self.inner.control()
    }
}

impl<S> HostedDeck<S> for ObservedQueue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    delegate::delegate! {
        to self.inner {
            fn worker(&self) -> Option<&PlayWorker<S>>;
            fn resource_prep(&self) -> Option<&ResourcePrep<S>>;
            fn mixer_config(&self) -> DeckMixerConfig;
            fn close(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError>;
            fn release(&mut self);
        }
    }

    fn hold(&mut self, waker: Waker) {
        self.commands.lock().wake = Some(waker.clone());
        self.inner.hold(waker);
    }

    fn drain(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        self.publish_consumed(pass);
        self.inner.drain(pass, out);
        let commands = std::mem::take(&mut self.commands.lock().commands);
        for command in commands {
            match command {
                LinkProbeCommand::ReadSnapshot { answer } => {
                    let snapshot = self
                        .inner
                        .tracks_active()
                        .filter_map(|track| {
                            let snapshot = track.snapshot();
                            let slot = snapshot.track.slot?;
                            let consumed = pass.deck.slots.get(usize::from(slot.get())).copied()?;
                            Some((snapshot, consumed))
                        })
                        .collect();
                    let _ = answer.send(snapshot);
                }
                LinkProbeCommand::Track { item, command } => {
                    let track = self
                        .inner
                        .tracks_mut()
                        .find(|track| track.snapshot().track.item == item)
                        .expect("probe targets an active track");
                    if let Some(seq) = track
                        .apply(command, out)
                        .expect("real track command admitted")
                    {
                        self.observation.lock().commands.push((item, seq));
                    }
                }
                LinkProbeCommand::Incoming { item, source, cue } => {
                    self.inner.factory_mut().set_synced(true);
                    self.inner
                        .apply(
                            QueueCommand::Append {
                                id: item,
                                source,
                                position: Position::ZERO,
                            },
                            out,
                        )
                        .expect("append incoming track");
                    self.inner
                        .apply(
                            QueueCommand::Select {
                                id: item,
                                transition: Transition::None,
                            },
                            out,
                        )
                        .expect("select incoming track");
                    if let Some(cue) = cue {
                        let track = self
                            .inner
                            .tracks_mut()
                            .find(|track| track.snapshot().track.item == item)
                            .expect("selected incoming track");
                        track
                            .apply(TrackCommand::Seek { to: cue }, out)
                            .expect("incoming cue waits for its grid");
                    }
                }
            }
        }
        self.publish();
    }
    fn settle(
        &mut self,
        receipt: TrackReceipt<'_, S>,
        pass: DeckPass<'_>,
        out: &mut Outbox<'_, S>,
    ) {
        if let TrackReceipt::Loaded(receipt) = &receipt {
            let verdict = match receipt.outcome() {
                Outcome::Applied { .. } => LinkVerdict::Applied,
                Outcome::Rejected(Rejection::Refused(LoadRefusal::Capacity { .. })) => {
                    LinkVerdict::Capacity
                }
                Outcome::Rejected(Rejection::Refused(LoadRefusal::Cancelled)) => {
                    LinkVerdict::Cancelled
                }
                Outcome::Rejected(_) => LinkVerdict::Refused,
            };
            self.observation
                .lock()
                .dispatcher
                .push((receipt.seq(), verdict));
        }
        if let TrackReceipt::Deck { seq, outcome, .. } = &receipt {
            let verdict = match outcome {
                Outcome::Applied { .. } => LinkVerdict::Applied,
                Outcome::Rejected(Rejection::Stale) => LinkVerdict::Stale,
                Outcome::Rejected(_) => LinkVerdict::Refused,
            };
            self.observation.lock().deck.push((*seq, verdict));
        }
        let capturing = self.observation.lock().capture.is_some();
        if capturing {
            self.capture_native(&receipt);
            self.publish_consumed(pass);
            self.capture_terminal(&receipt);
        }
        HostedDeck::settle(&mut self.inner, receipt, pass, out);
        self.publish_pass(pass);
    }
    fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        HostedDeck::tick(&mut self.inner, pass, out);
        self.publish_pass(pass);
    }
}

impl<S> Player<S> for ObservedQueue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Command = QueueCommand<S>;
    type Snapshot = kithara::queue::QueueSnapshot<S>;

    fn entry(&self, bound: Bound) -> Option<SessionFrame> {
        Player::entry(&self.inner, bound)
    }
    fn apply(
        &mut self,
        command: Self::Command,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let seq = Player::apply(&mut self.inner, command, out)?;
        self.publish();
        Ok(seq)
    }
    fn settle(&mut self, receipt: TrackReceipt<'_, S>, out: &mut Outbox<'_, S>) -> Settled {
        let settled = Player::settle(&mut self.inner, receipt, out);
        self.publish();
        settled
    }
    fn tick(&mut self, now: SessionFrame, out: &mut Outbox<'_, S>) {
        Player::tick(&mut self.inner, now, out);
        self.publish();
    }
    fn snapshot(&self) -> Self::Snapshot {
        Player::snapshot(&self.inner)
    }
}

impl<S> LinkedPlayer<S> for ObservedQueue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    fn sync(&mut self, on: bool, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError> {
        let (input, start) = {
            let mut observation = self.observation.lock();
            let input = observation.owner_inputs.len();
            let start = observation.placements.len();
            observation.owner_inputs.push(LinkOwnerInput {
                item: None,
                at: out.pass().map(|pass| pass.now),
                trajectory: None,
                kind: LinkOwnerInputKind::Sync { on },
            });
            drop(observation);
            (input, start)
        };
        let seq = LinkedPlayer::sync(&mut self.inner, on, out)?;
        let tracks = self.inner.tracks_active().map(Player::snapshot).collect();
        {
            let mut observation = self.observation.lock();
            let end = observation.placements.len();
            observation.sync_requests.push(LinkSyncRequest {
                on,
                seq,
                input,
                tracks,
                placements: start..end,
            });
        }
        self.publish();
        Ok(seq)
    }
    fn retime(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>) {
        self.observation.lock().owner_inputs.push(LinkOwnerInput {
            item: None,
            at: Some(at),
            trajectory: Some(trajectory.clone()),
            kind: LinkOwnerInputKind::Retime,
        });
        LinkedPlayer::retime(&mut self.inner, trajectory, at, out);
        self.publish();
    }
    fn grid(&mut self, answer: GridAnswer, out: &mut Outbox<'_, S>) {
        self.observation.lock().owner_inputs.push(LinkOwnerInput {
            item: Some(answer.item),
            at: None,
            trajectory: None,
            kind: LinkOwnerInputKind::Grid {
                answer: Box::new(answer.clone()),
            },
        });
        LinkedPlayer::grid(&mut self.inner, answer, out);
        self.publish();
    }
    fn synced(&self) -> bool {
        LinkedPlayer::synced(&self.inner)
    }
    fn lead(&self, delivery: FrameCount) -> Option<FrameCount> {
        LinkedPlayer::lead(&self.inner, delivery)
    }
    fn lane_room(&self) -> usize {
        LinkedPlayer::lane_room(&self.inner)
    }
    fn scope_parts(&self) -> usize {
        LinkedPlayer::scope_parts(&self.inner)
    }
    fn retime_applied(&mut self, at: SessionFrame) -> Option<bool> {
        LinkedPlayer::retime_applied(&mut self.inner, at)
    }
    fn realign(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>) {
        self.observation.lock().owner_inputs.push(LinkOwnerInput {
            item: None,
            at: Some(at),
            trajectory: Some(trajectory.clone()),
            kind: LinkOwnerInputKind::Realign,
        });
        LinkedPlayer::realign(&mut self.inner, trajectory, at, out);
        self.publish();
    }
}

impl<S> OfflineHostHarness<S, LinkedOwner<S>>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub async fn insert_linked(
        &self,
        queue: LinkedQueue<S>,
        observation: Arc<Mutex<LinkObservation>>,
    ) -> Result<LinkedQueueHandle<S>, PlayError> {
        self.with(move |host| {
            let commands = Arc::new(Mutex::new(LinkProbes {
                commands: Vec::new(),
                wake: None,
            }));
            let deck = host.insert(ObservedQueue {
                inner: queue,
                observation: observation.clone(),
                commands: commands.clone(),
            })?;
            Ok(LinkedQueueHandle {
                id: deck.id(),
                control: deck.control().clone(),
                observation,
                commands,
            })
        })
        .await
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[kithara::test]
    fn speed_trace_preserves_admitted_and_rejected_inputs() {
        let (posts, _mailbox) = kithara_command::mailbox::<(), ()>();
        let seq = posts.post(()).expect("real command identity").seq();
        let curve = kithara::sync::SpeedCurve::ApproachSteps(
            vec![(
                0,
                kithara::signal::ExponentialApproach::new(0.9, 1.1, 240.0)
                    .expect("finite command curve"),
            )]
            .into(),
        );
        assert_eq!(
            speed_trace_evidence(&Ok(Some(seq)), &curve, 7),
            serde_json::json!({"input": 7})
        );
        for result in [Ok(None), Err(PlayError::Closed)] {
            assert_eq!(
                speed_trace_evidence(&result, &curve, 7),
                serde_json::json!(format!("{curve:?}"))
            );
        }
        let item = TrackId::allocate();
        let track: ObservedTrack<crate::bufpool_ext::TestPools> =
            ObservedFactory::new(Arc::new(Mutex::new(LinkObservation::default())))
                .track(PlayerConfig {
                    item,
                    slot: None,
                    settings: TrackSettings::default(),
                })
                .expect("diagnostic owner track");
        let entry = serde_json::json!({
            "event": "command", "item": u64::from(item),
            "details": {"SetSpeed": {"speed": {"input": 0}}},
        });
        let observation = LinkObservation {
            timeline: vec![entry.clone()],
            speeds: vec![LinkSpeed {
                item,
                seq,
                curve: curve.clone(),
                at: When::Next,
                landing: None,
                before: track.snapshot(),
                consumed: None,
                sample_rate: None,
                input: None,
                host_input: None,
                planned: None,
            }],
            ..LinkObservation::default()
        };
        let standalone = observation.timeline_evidence().collect::<Vec<_>>();
        assert_eq!(
            standalone[0]["details"]["SetSpeed"]["speed"],
            serde_json::json!(format!("{curve:?}"))
        );
        assert_eq!(
            observation.timeline[0], entry,
            "diagnostic expansion does not mutate retained facts"
        );
    }
}
