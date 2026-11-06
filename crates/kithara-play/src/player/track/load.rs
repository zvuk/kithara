use kithara_command::{Outcome, Receipt, Rejection, Seq, Target, When};
use kithara_config::ConfigOwner;
use kithara_render::{
    Dispatched, DispatcherProtocol, LoadRequest, Loaded, ServiceClass,
    bridge::{DeckPart, Fade, Slot, SlotState},
};
use kithara_signal::{SegmentId, SessionFrame};
use tracing::warn;

use super::{
    super::outbox::{Outbox, Settled, rejection},
    core::PlayerImpl,
    state::*,
};
use crate::{OpenedTrack, PlayError, ResourceLoad};

impl<S> PlayerImpl<S> {
    pub(super) fn load(
        &mut self,
        item: ResourceLoad<S>,
        position: Position,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if self.status != TrackStatus::Idle {
            return Err(PlayError::Internal("a track loads once".into()));
        }
        if out.dispatcher_available() == 0 {
            return Err(PlayError::Full("dispatcher"));
        }
        let (lane, inbox) = item.lane_channel()?;
        let (ring_depth, declick) = item.lane_geometry()?;
        self.class = if self.slot.is_some() {
            ServiceClass::Warm
        } else {
            ServiceClass::Idle
        };
        let seq = out.load(LoadRequest {
            item,
            class: self.class,
            position,
            start: self.settings.config().lane_start(),
            inbox,
        })?;
        self.lane = Some(lane);
        self.ring_depth = ring_depth;
        self.declick = declick;
        self.position = position;
        self.segment_speed = self.settings.config().speed();
        self.segment_origin = None;
        self.source_basis = None;
        self.observed_source = None;
        self.loading = Some(Loading {
            seq,
            opened: None,
            speed: None,
        });
        self.status = TrackStatus::Loading;
        Ok(Some(seq))
    }

    pub(super) fn opened(
        &mut self,
        receipt: Receipt<DispatcherProtocol<ResourceLoad<S>>>,
        out: &mut Outbox<'_, S>,
    ) -> Settled {
        let seq = receipt.seq();
        if self.release == Some(seq) {
            self.settle_release(&receipt);
            return Settled::Pending;
        }
        if !self
            .loading
            .as_ref()
            .is_some_and(|loading| loading.seq == seq)
        {
            return Settled::Pending;
        }
        let (outcome, _) = receipt.into();
        match outcome {
            Outcome::Applied {
                data: Dispatched::Loaded(loaded),
                ..
            } => self.accept_opened(seq, loaded, out),
            Outcome::Rejected(refused) => {
                self.loading = None;
                self.lane = None;
                if !matches!(self.status, TrackStatus::Failed { .. }) {
                    self.status = TrackStatus::Idle;
                }
                Settled::Rejected {
                    seq,
                    reason: rejection(&refused, |refusal| PlayError::ItemFailed {
                        reason: refusal.to_string(),
                    }),
                }
            }
            Outcome::Applied { .. } => Settled::Rejected {
                seq,
                reason: Rejection::Refused(PlayError::Internal(
                    "a load received a non-load answer".into(),
                )),
            },
        }
    }

    fn accept_opened(
        &mut self,
        seq: Seq,
        loaded: Loaded<OpenedTrack>,
        out: &mut Outbox<'_, S>,
    ) -> Settled {
        self.lane_id = Some(loaded.lane);
        self.engine_latency = loaded.engine_latency;
        if self.segment == SegmentId::FIRST {
            self.ready = Some(SegmentId::FIRST);
        }
        let prepared = loaded.opened.pcm.next_chunk_info();
        if let Some(meta) = prepared.filter(|meta| meta.segment == self.segment) {
            self.segment_origin = Some(meta.timestamp);
        }
        self.source_basis = prepared;
        self.observed_source = prepared;
        self.duration = prepared.and_then(|meta| meta.media_end);
        self.visible_origin = prepared.and_then(|meta| meta.visible_origin);
        self.media_origin = prepared.and_then(|meta| meta.media_origin);
        self.abr = loaded.opened.abr.clone();
        self.metadata = loaded.opened.metadata.clone();
        if let Some(loading) = self.loading.as_mut() {
            loading.opened = Some(loaded.opened);
        }
        if self.status == TrackStatus::Released || self.preparation_failure().is_some() {
            self.loading = None;
            if let Err(error) = self.release_lane(out) {
                warn!(%error, "released open waits for dispatcher room");
            }
            return Settled::Pending;
        }
        self.reserved_ready();
        let speed = self
            .loading
            .as_mut()
            .and_then(|loading| loading.speed.take());
        let entry = match speed {
            Some((speed, _)) => prepared
                .map(|meta| meta.timestamp)
                .ok_or(PlayError::Untimed)
                .and_then(|from| self.segment_with(from, None, speed, out))
                .map(drop),
            None => Ok(()),
        };
        if let Err(error) = entry.and_then(|()| self.attach(out)) {
            self.loading = None;
            self.lane = None;
            self.status = TrackStatus::Idle;
            if let Err(error) = self.release_lane(out) {
                warn!(%error, "refused open waits for dispatcher room");
            }
            return Settled::Rejected {
                seq,
                reason: Rejection::Refused(error),
            };
        }
        self.promote(out);
        Settled::Pending
    }

    pub(super) fn settle_release(
        &mut self,
        receipt: &Receipt<DispatcherProtocol<ResourceLoad<S>>>,
    ) {
        self.release = None;
        match receipt.outcome() {
            Outcome::Applied {
                data: Dispatched::Released,
                ..
            } => {
                self.lane = None;
                self.lane_id = None;
            }
            Outcome::Rejected(reason) => warn!(?reason, "dispatcher refused lane release"),
            Outcome::Applied { .. } => {
                let seq = receipt.seq();
                warn!(?seq, "lane release received a non-release answer");
            }
        }
    }

    pub(super) fn attach(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError> {
        if out.is_grouped() || self.attaching.is_some() {
            return Ok(());
        }
        let Some(loading) = self.loading.as_mut() else {
            return Ok(());
        };
        let Some(slot) = self.slot else {
            return Ok(());
        };
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        let Some(opened) = loading.opened.take() else {
            return Ok(());
        };
        let OpenedTrack { pcm, .. } = opened;
        let seq = loading.seq;
        let at = When::Next;
        let mut parts = vec![DeckPart::Attach {
            slot,
            item_id: self.item,
            pcm,
            segment: self.segment,
        }];
        let start = self.play.is_some() && self.ready == Some(self.segment);
        if start {
            parts.push(DeckPart::Start {
                slot,
                fade: Fade::Declick,
            });
        }
        match out.deck_owned(at, parts) {
            Ok(attach) => {
                self.attaching = Some(Attaching {
                    seq: attach,
                    caller: seq,
                    at,
                    replacement: false,
                    play: if start { self.play.take() } else { None },
                    obsolete: false,
                });
                Ok(())
            }
            Err((error, mut parts)) => {
                self.restore_attachment(&mut parts);
                Err(error)
            }
        }
    }

    pub(super) fn restore_attachment(&mut self, parts: &mut Vec<DeckPart>) -> bool {
        let Some(loading) = self.loading.as_mut() else {
            return false;
        };
        let Some(index) = parts.iter().position(|part| {
            matches!(part,
                DeckPart::Attach { slot, .. } | DeckPart::Replace { slot, .. } if Some(*slot) == self.slot
            )
        }) else {
            return false;
        };
        let (DeckPart::Attach { pcm, .. } | DeckPart::Replace { pcm, .. }) = parts.remove(index)
        else {
            unreachable!("the selected attachment carries PCM");
        };
        loading.opened = Some(OpenedTrack {
            pcm,
            abr: self.abr.clone(),
            metadata: self.metadata.clone(),
        });
        true
    }

    pub(super) fn reserved_ready(&mut self) {
        if !self.preparation_closed()
            && self.slot.is_none()
            && self.ready == Some(self.segment)
            && self
                .loading
                .as_ref()
                .is_some_and(|loading| loading.opened.is_some())
        {
            self.status = TrackStatus::Loaded;
        }
    }

    pub(super) fn seat_at(
        &mut self,
        slot: Slot,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if matches!(at, When::Deferred) {
            return Err(PlayError::Internal(
                "a seat needs a timed deck batch".into(),
            ));
        }
        if self.slot.is_some() {
            return Err(PlayError::Internal("a track is seated once".into()));
        }
        if at == When::Next {
            return self.seat_next(slot, out);
        }
        if self.attaching.is_some() || self.ready != Some(self.segment) {
            return Err(PlayError::NotReady);
        }
        let pass = out.pass().ok_or(PlayError::Untimed)?;
        if !pass
            .deck
            .slots
            .get(slot.index())
            .is_some_and(|slot| slot.state != SlotState::Empty)
        {
            return Err(PlayError::NotReady);
        }
        let loading = self.loading.as_mut().ok_or(PlayError::NotReady)?;
        let opened = loading.opened.take().ok_or(PlayError::NotReady)?;
        let OpenedTrack { pcm, abr, metadata } = opened;
        match out.deck_owned(
            at,
            vec![DeckPart::Replace {
                slot,
                item_id: self.item,
                pcm,
                segment: self.segment,
            }],
        ) {
            Ok(seq) => {
                self.slot = Some(slot);
                self.attaching = Some(Attaching {
                    seq,
                    caller: seq.unwrap_or(loading.seq),
                    at,
                    replacement: true,
                    play: self.play.take(),
                    obsolete: false,
                });
                self.promote(out);
                Ok(seq)
            }
            Err((error, parts)) => {
                if let Some(pcm) = parts.into_iter().find_map(|part| match part {
                    DeckPart::Replace {
                        slot: named, pcm, ..
                    } if named == slot => Some(pcm),
                    _ => None,
                }) {
                    loading.opened = Some(OpenedTrack { pcm, abr, metadata });
                } else {
                    return Err(PlayError::Internal(
                        "replacement refusal did not return its original PCM".into(),
                    ));
                }
                self.slot = None;
                self.reserved_ready();
                Err(error)
            }
        }
    }

    fn seat_next(&mut self, slot: Slot, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError> {
        if self
            .loading
            .as_ref()
            .is_none_or(|loading| loading.opened.is_none())
        {
            self.slot = Some(slot);
            return Ok(None);
        }
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        self.slot = Some(slot);
        self.status = TrackStatus::Loading;
        if let Err(error) = self.attach(out) {
            self.slot = None;
            self.reserved_ready();
            return Err(error);
        }
        self.promote(out);
        Ok(None)
    }

    pub(super) fn promote(&mut self, out: &mut Outbox<'_, S>) {
        let class = match self.status {
            TrackStatus::Playing { .. } => ServiceClass::Audible,
            TrackStatus::Ended { .. } | TrackStatus::Faded { .. } => ServiceClass::Idle,
            _ if self.slot.is_some() && self.class == ServiceClass::Idle => ServiceClass::Warm,
            _ => return,
        };
        if self.class != class
            && let Some(lane) = self.lane_id
        {
            match out.prioritize(lane, class) {
                Ok(_) => self.class = class,
                Err(error) => warn!(%error, "a track waits to publish its service class"),
            }
        }
    }
}
