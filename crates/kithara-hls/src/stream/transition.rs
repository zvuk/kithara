mod prepare;

use std::io::{Error as IoError, ErrorKind};

use arc_swap::ArcSwap;
use kithara_abr::{AbrDecision, PendingAbrClaim, PendingAbrDecision};
use kithara_events::{AbrReason, SeekEpoch, VariantIndex};
use kithara_platform::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use kithara_stream::{
    OpenedVariantReader, OutgoingDisposition, ReaderProfile, SourceError, StreamError,
    StreamResult, VariantPromotion, VariantReaderPlan, VariantReaderTake, VariantTransition,
    VariantTransitionId,
};
use kithara_test_utils::kithara;
use tracing::debug;

use super::{
    coord::{HlsCoord, variant_switch_target_time},
    session::HlsSession,
};
pub(super) struct SessionSlots {
    publication: ArcSwap<ResidentSessions>,
    transition: Mutex<TransitionState>,
}

struct ResidentSessions {
    first: Arc<HlsSession>,
    second: Option<Arc<HlsSession>>,
}

impl ResidentSessions {
    fn find(&self, variant_index: usize) -> Option<&Arc<HlsSession>> {
        if self.first.variant_index() == variant_index {
            return Some(&self.first);
        }
        self.second
            .as_ref()
            .filter(|session| session.variant_index() == variant_index)
    }

    const fn one(session: Arc<HlsSession>) -> Self {
        Self {
            first: session,
            second: None,
        }
    }

    const fn two(first: Arc<HlsSession>, second: Arc<HlsSession>) -> Self {
        Self {
            first,
            second: Some(second),
        }
    }
}

impl SessionSlots {
    pub(super) fn new(active: Arc<HlsSession>) -> Self {
        Self {
            publication: ArcSwap::from_pointee(ResidentSessions::one(active)),
            transition: Mutex::new(TransitionState { incoming: None }),
        }
    }

    pub(super) fn active(&self, mut selected_variant: impl FnMut() -> usize) -> Arc<HlsSession> {
        loop {
            let before = selected_variant();
            let residents = self.publication.load();
            let resolved = residents.find(before);
            let after = selected_variant();
            if before != after {
                continue;
            }
            return resolved.map_or_else(
                || panic!("exact resident sessions do not contain published ABR variant {before}"),
                Arc::clone,
            );
        }
    }

    pub(super) fn incoming_session(&self) -> Option<Arc<HlsSession>> {
        self.transition
            .lock()
            .incoming
            .as_ref()
            .map(|slot| Arc::clone(&slot.session))
    }

    fn publish_exact_one(&self, session: Arc<HlsSession>) {
        self.publication
            .store(Arc::new(ResidentSessions::one(session)));
    }

    fn publish_exact_two(&self, first: Arc<HlsSession>, second: Arc<HlsSession>) {
        self.publication
            .store(Arc::new(ResidentSessions::two(first, second)));
    }

    #[cfg(test)]
    pub(super) fn resident_count(&self) -> usize {
        if self.publication.load().second.is_some() {
            2
        } else {
            1
        }
    }
}

struct TransitionState {
    incoming: Option<IncomingSlot>,
}

struct IncomingSlot {
    session: Arc<HlsSession>,
    landing_time: Duration,
    reader: Option<OpenedVariantReader>,
    claim: PendingAbrDecision,
    profile: ReaderProfile,
    transition: VariantTransition,
}

impl HlsCoord {
    pub(super) fn abort_variant(&self, transition: VariantTransition) -> bool {
        let mut state = self.sessions.transition.lock();
        if state
            .incoming
            .as_ref()
            .is_none_or(|slot| slot.transition != transition)
        {
            return false;
        }
        self.discard_incoming(&mut state, true);
        drop(state);
        true
    }

    pub(crate) fn active_session(&self) -> Arc<HlsSession> {
        self.sessions.active(|| {
            self.abr
                .current_variant_index()
                .expect("BUG: HLS coordinator lost its stateful ABR selector")
        })
    }

    pub(super) fn cancel_incoming_for_seek(&self) {
        let mut state = self.sessions.transition.lock();
        self.discard_incoming(&mut state, false);
    }

    #[kithara::probe(abort_intent)]
    fn discard_incoming(&self, state: &mut TransitionState, abort_intent: bool) {
        let Some(slot) = state.incoming.take() else {
            return;
        };
        // With `abort_intent` the pending decision dies with the slot and no
        // tick source re-derives it — this line is the only witness of a
        // switch that ends without ever committing.
        debug!(
            transition = ?slot.transition,
            abort_intent,
            reader_untaken = slot.reader.is_some(),
            "discarding incoming variant session"
        );
        let active = self.active_session();
        self.sessions.publish_exact_one(active);
        slot.session.abort();
        if abort_intent {
            let _ = self.abr_publisher.abort_pending(slot.claim.ticket());
        }
    }

    /// Fetches for the variant that is audible right now.
    ///
    /// While an incoming slot exists the audible session is held to its owed
    /// window: its look-ahead was retired when the slot was installed, and
    /// letting the next poll refill it would starve the construction all over
    /// again — the downloader runs a queued pack only when a slot frees.
    pub(crate) fn dispatch_active(
        &self,
        ctx: &crate::variant::PlanCtx,
        budget: usize,
    ) -> Vec<kithara_stream::dl::FetchCmd> {
        let session = self.active_session();
        if self.has_incoming() {
            return session.dispatch_owed(ctx, budget);
        }
        session.dispatch(ctx, budget)
    }

    /// Fetches for the variant a switch is preparing.
    ///
    /// Construction fetches are tagged `High`. The peer's budget decides how
    /// many a poll may emit; it cannot decide when the downloader runs them, and
    /// that queue is first-in-first-out within a priority slot. The audible
    /// variant appends to it every poll, so an untagged construction pack is
    /// never reached — measured on a cross-codec switch, the incoming variant
    /// started only its playlist and its init all test while both media segments
    /// sat queued until teardown.
    ///
    /// The slot owning the reader is what says whether the session is still
    /// under construction: while `reader` is held the session has no reader
    /// position to plan against and stays capped at its construction window;
    /// once the reader is transferred its decoder is priming and the session
    /// must serve that decoder's reads.
    pub(crate) fn dispatch_incoming(
        &self,
        ctx: &crate::variant::PlanCtx,
        budget: usize,
    ) -> Vec<kithara_stream::dl::FetchCmd> {
        let incoming = self
            .sessions
            .transition
            .lock()
            .incoming
            .as_ref()
            .map(|slot| (Arc::clone(&slot.session), slot.reader.is_some()));
        match incoming {
            None => Vec::new(),
            Some((session, true)) => session.dispatch_constructing(ctx, budget),
            Some((session, false)) => session.dispatch(ctx, budget),
        }
    }

    pub(crate) fn has_incoming(&self) -> bool {
        self.sessions.transition.lock().incoming.is_some()
    }

    #[kithara::probe(has_incoming = self.has_incoming())]
    pub(super) fn plan_variant_reader(
        &self,
        landing: Option<Duration>,
    ) -> StreamResult<Option<VariantReaderPlan>> {
        let mut state = self.sessions.transition.lock();
        let claim = match self.abr.pending_claim() {
            PendingAbrClaim::Absent => {
                self.discard_incoming(&mut state, true);
                return Ok(None);
            }
            PendingAbrClaim::Locked(claim) => {
                if let Some(slot) = state.incoming.as_ref()
                    && slot.claim == claim
                {
                    let epoch_matches =
                        self.seek_observe().epoch() == slot.transition.id().seek_epoch();
                    let active_matches =
                        self.variant_index() == slot.transition.active_variant().get();
                    if !epoch_matches || !active_matches {
                        self.discard_incoming(&mut state, false);
                        return Ok(None);
                    }
                    return Ok(Some(VariantReaderPlan::new(
                        slot.transition,
                        slot.session.media_info(),
                        slot.landing_time,
                    )));
                }
                self.discard_incoming(&mut state, true);
                return Ok(None);
            }
            PendingAbrClaim::Ready(claim) => claim,
            _ => return Err(unsupported_pending_claim()),
        };
        let transition = transition_for_claim(
            claim,
            self.seek_observe().epoch(),
            VariantIndex::new(self.variant_index()),
        );

        if let Some(slot) = state.incoming.as_ref()
            && slot.transition == transition
        {
            return Ok(Some(VariantReaderPlan::new(
                transition,
                slot.session.media_info(),
                slot.landing_time,
            )));
        }
        if state.incoming.is_some() {
            let abort_intent = state
                .incoming
                .as_ref()
                .is_some_and(|slot| slot.claim != claim);
            self.discard_incoming(&mut state, abort_intent);
        }
        drop(state);

        let Some(target) = self.variants.get(transition.incoming_variant().get()) else {
            let _ = self.abr_publisher.abort_pending(claim.ticket());
            return Err(StreamError::Source(SourceError::VariantNotFound(format!(
                "incoming variant {}",
                transition.incoming_variant().get()
            ))));
        };
        let epoch_matches = self.seek_observe().epoch() == transition.id().seek_epoch();
        let claim_matches = match self.abr.pending_claim() {
            PendingAbrClaim::Ready(current) | PendingAbrClaim::Locked(current) => current == claim,
            _ => false,
        };
        if !epoch_matches
            || !claim_matches
            || self.variant_index() != transition.active_variant().get()
        {
            if epoch_matches {
                let _ = self.abr_publisher.abort_pending(claim.ticket());
            }
            return Ok(None);
        }

        let landing_time = variant_switch_target_time(
            self.seek_observe().as_ref(),
            self.playhead_read().as_ref(),
            landing,
        );
        Ok(Some(VariantReaderPlan::new(
            transition,
            target.media_info(),
            landing_time,
        )))
    }

    #[kithara::probe(
        active = transition.active_variant().get(),
        incoming = transition.incoming_variant().get()
    )]
    pub(super) fn promote_planned_variant(
        &self,
        transition: VariantTransition,
    ) -> VariantPromotion {
        let mut state = self.sessions.transition.lock();
        let Some(slot) = state.incoming.as_ref() else {
            return VariantPromotion::Stale;
        };
        if slot.transition != transition {
            return VariantPromotion::Stale;
        }
        if self.seek_observe().epoch() != transition.id().seek_epoch() {
            self.discard_incoming(&mut state, false);
            return VariantPromotion::Stale;
        }
        match self.abr.pending_claim() {
            PendingAbrClaim::Locked(claim) if claim == slot.claim => {
                return VariantPromotion::Deferred;
            }
            PendingAbrClaim::Locked(_) | PendingAbrClaim::Absent => {
                self.discard_incoming(&mut state, true);
                return VariantPromotion::Stale;
            }
            PendingAbrClaim::Ready(claim)
                if claim != slot.claim
                    || self.variant_index() != transition.active_variant().get() =>
            {
                self.discard_incoming(&mut state, true);
                return VariantPromotion::Stale;
            }
            PendingAbrClaim::Ready(_) => {}
            _ => return VariantPromotion::Stale,
        }
        if slot.reader.is_some() {
            return VariantPromotion::Deferred;
        }
        let Some(slot) = state.incoming.take() else {
            return VariantPromotion::Stale;
        };
        let outgoing = self.active_session();
        let now = Instant::now();
        let committed = self.commit_if_seek_epoch(transition.id().seek_epoch(), || {
            if !self.abr_publisher.commit_pending(slot.claim, now) {
                return false;
            }
            slot.session.activate();
            self.sessions.publish_exact_one(Arc::clone(&slot.session));
            outgoing.abort();
            true
        });
        match committed {
            None => {
                self.sessions.publish_exact_one(outgoing);
                slot.session.abort();
                return VariantPromotion::Stale;
            }
            Some(false) => {
                let claim_matches = match self.abr.pending_claim() {
                    PendingAbrClaim::Ready(current) | PendingAbrClaim::Locked(current) => {
                        current == slot.claim
                    }
                    _ => false,
                };
                let epoch_matches = self.seek_observe().epoch() == transition.id().seek_epoch();
                let active_matches = self.variant_index() == transition.active_variant().get();
                if claim_matches && epoch_matches && active_matches {
                    state.incoming = Some(slot);
                    return VariantPromotion::Deferred;
                }
                self.sessions.publish_exact_one(outgoing);
                slot.session.abort();
                return VariantPromotion::Stale;
            }
            Some(true) => {}
        }
        drop(state);
        debug!(?transition, "variant transition promoted");
        self.abr
            .notify_exact_commit(slot.claim.decision(), transition.active_variant().get());
        self.signal().fire();
        VariantPromotion::Promoted
    }

    /// The reader's own consumption re-opening a deferred prefetch decision.
    /// Answered by the session, which owns the byte cursor; the variant only
    /// owns the threshold.
    pub(crate) fn take_prefetch_resume(&self) -> bool {
        self.active_session().take_prefetch_resume()
    }

    pub(super) fn take_prepared_variant_reader(
        &self,
        transition: VariantTransition,
    ) -> StreamResult<VariantReaderTake> {
        let mut wake_when_unlocked: Option<Arc<HlsSession>> = None;
        let mut state = self.sessions.transition.lock();
        let result = if let Some(slot) = state.incoming.as_mut() {
            if slot.transition != transition {
                Ok(VariantReaderTake::Stale)
            } else if self.seek_observe().epoch() != transition.id().seek_epoch() {
                self.discard_incoming(&mut state, false);
                Ok(VariantReaderTake::Stale)
            } else {
                match self.abr.pending_claim() {
                    PendingAbrClaim::Locked(claim)
                        if claim == slot.claim
                            && self.variant_index() == transition.active_variant().get() =>
                    {
                        Ok(VariantReaderTake::Preparing)
                    }
                    PendingAbrClaim::Locked(_) | PendingAbrClaim::Absent => {
                        self.discard_incoming(&mut state, true);
                        Ok(VariantReaderTake::Stale)
                    }
                    PendingAbrClaim::Ready(claim)
                        if claim != slot.claim
                            || self.variant_index() != transition.active_variant().get() =>
                    {
                        self.discard_incoming(&mut state, true);
                        Ok(VariantReaderTake::Stale)
                    }
                    PendingAbrClaim::Ready(_) => match slot.session.is_ready() {
                        Ok(false) => {
                            // Owed a wake, but not from here: `wake_peer` takes
                            // the peer's state lock, and the peer holds that
                            // across `prepare_for_seek`, which takes the
                            // transition lock this arm is standing on.
                            wake_when_unlocked = Some(Arc::clone(&slot.session));
                            Ok(VariantReaderTake::Preparing)
                        }
                        Ok(true) => Ok(slot
                            .reader
                            .take()
                            .map_or(VariantReaderTake::Taken, VariantReaderTake::Ready)),
                        Err(error) => {
                            self.discard_incoming(&mut state, true);
                            Err(error)
                        }
                    },
                    _ => Err(unsupported_pending_claim()),
                }
            }
        } else {
            Ok(VariantReaderTake::Stale)
        };
        drop(state);
        if let Some(session) = wake_when_unlocked {
            session.wake_peer_for_readiness();
        }
        result
    }
}

fn transition_for_claim(
    claim: PendingAbrDecision,
    seek_epoch: SeekEpoch,
    active_variant: VariantIndex,
) -> VariantTransition {
    let outgoing_disposition = match claim.decision() {
        AbrDecision::UpSwitch {
            reason: AbrReason::EscapeStalled,
            ..
        } => OutgoingDisposition::Abandoned,
        _ => OutgoingDisposition::Retained,
    };
    VariantTransition::new(
        VariantTransitionId::new(claim.ticket(), seek_epoch),
        active_variant,
        claim.decision().target(),
    )
    .with_outgoing_disposition(outgoing_disposition)
}

fn unsupported_pending_claim() -> StreamError {
    StreamError::Source(SourceError::Io(IoError::new(
        ErrorKind::InvalidData,
        "unsupported pending ABR claim state",
    )))
}
