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
    seat::{Attaching, Seat},
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
        let geometry = item.lane_geometry()?;
        self.class = if self.seat.slot().is_some() {
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
        self.geometry = geometry;
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
        if matches!(self.registration, Registration::Releasing { seq: release, .. } if release == seq)
        {
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
        self.registration = Registration::Registered(loaded.lane);
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
        self.extent = Extent::of(prepared);
        let OpenedTrack { pcm, abr, metadata } = loaded.opened;
        self.abr = abr;
        self.metadata = metadata;
        if let Some(loading) = self.loading.as_mut() {
            loading.opened = Some(pcm);
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
        let Registration::Releasing { lane, .. } = self.registration else {
            return;
        };
        self.registration = Registration::Registered(lane);
        match receipt.outcome() {
            Outcome::Applied {
                data: Dispatched::Released,
                ..
            } => {
                self.lane = None;
                self.registration = Registration::None;
            }
            Outcome::Rejected(reason) => warn!(?reason, "dispatcher refused lane release"),
            Outcome::Applied { .. } => {
                let seq = receipt.seq();
                warn!(?seq, "lane release received a non-release answer");
            }
        }
    }

    pub(super) fn attach(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError> {
        if out.is_grouped() || self.seat.attaching().is_some() {
            return Ok(());
        }
        let Some(loading) = self.loading.as_mut() else {
            return Ok(());
        };
        let Some(slot) = self.seat.slot() else {
            return Ok(());
        };
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        let Some(pcm) = loading.opened.take() else {
            return Ok(());
        };
        let seq = loading.seq;
        let at = When::Next;
        let mut parts = self.seated_with_clock(
            slot,
            vec![DeckPart::Attach {
                slot,
                item_id: self.item,
                pcm,
                segment: self.segment,
            }],
        );
        let start = self.play.is_some() && self.ready == Some(self.segment);
        if start {
            parts.push(DeckPart::Start {
                slot,
                fade: Fade::Declick,
            });
        }
        match out.deck_owned(at, parts) {
            Ok(attach) => {
                self.seat = Seat::Attaching {
                    slot,
                    attach: Attaching {
                        seq: attach,
                        caller: seq,
                        at,
                        replacement: false,
                        play: if start { self.play.take() } else { None },
                        obsolete: false,
                    },
                    releasing: false,
                };
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
                DeckPart::Attach { slot, .. } | DeckPart::Replace { slot, .. } if Some(*slot) == self.seat.slot()
            )
        }) else {
            return false;
        };
        let (DeckPart::Attach { pcm, .. } | DeckPart::Replace { pcm, .. }) = parts.remove(index)
        else {
            unreachable!("the selected attachment carries PCM");
        };
        loading.opened = Some(pcm);
        true
    }

    pub(super) fn reserved_ready(&mut self) {
        if !self.preparation_closed()
            && self.seat.slot().is_none()
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
        if self.seat.slot().is_some() {
            return Err(PlayError::Internal("a track is seated once".into()));
        }
        if at == When::Next {
            return self.seat_next(slot, out);
        }
        if self.ready != Some(self.segment) {
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
        let pcm = loading.opened.take().ok_or(PlayError::NotReady)?;
        let caller = loading.seq;
        let parts = self.seated_with_clock(
            slot,
            vec![DeckPart::Replace {
                slot,
                item_id: self.item,
                pcm,
                segment: self.segment,
            }],
        );
        match out.deck_owned(at, parts) {
            Ok(seq) => {
                self.seat = Seat::Attaching {
                    slot,
                    attach: Attaching {
                        seq,
                        caller: seq.unwrap_or(caller),
                        at,
                        replacement: true,
                        play: self.play.take(),
                        obsolete: false,
                    },
                    releasing: false,
                };
                self.promote(out);
                Ok(seq)
            }
            Err((error, parts)) => {
                if let Some(pcm) = parts.into_iter().find_map(|part| match part {
                    DeckPart::Replace {
                        slot: named, pcm, ..
                    } if named == slot => Some(pcm),
                    _ => None,
                }) && let Some(loading) = self.loading.as_mut()
                {
                    loading.opened = Some(pcm);
                } else {
                    return Err(PlayError::Internal(
                        "replacement refusal did not return its original PCM".into(),
                    ));
                }
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
            self.seat = Seat::Named(slot);
            return Ok(None);
        }
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        self.seat = Seat::Named(slot);
        self.status = TrackStatus::Loading;
        if let Err(error) = self.attach(out) {
            self.seat = Seat::Unseated;
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
            _ if self.seat.slot().is_some() && self.class == ServiceClass::Idle => {
                ServiceClass::Warm
            }
            _ => return,
        };
        if self.class != class
            && let Some(lane) = self.registration.lane()
        {
            match out.prioritize(lane, class) {
                Ok(_) => self.class = class,
                Err(error) => warn!(%error, "a track waits to publish its service class"),
            }
        }
    }
}
