use kithara_abr::{AbrTicket, VariantIndex};

/// Result of publishing an audio-approved incoming variant.
#[must_use]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum VariantPromotion {
    /// The exact ABR ticket and source session were published.
    Promoted,
    /// The transition is still exact, but publication is temporarily locked or
    /// its move-only reader has not been transferred yet.
    Deferred,
    /// The transition was superseded, aborted, or already promoted.
    Stale,
}

/// Exact identity of one variant transition for one accepted ABR request.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct VariantTransitionId {
    abr_ticket: AbrTicket,
}

impl VariantTransitionId {
    /// Bind an accepted ABR request to its source transition.
    #[must_use]
    pub const fn new(abr_ticket: AbrTicket) -> Self {
        Self { abr_ticket }
    }

    /// Accepted ABR request carried by this transition.
    #[must_use]
    pub const fn abr_ticket(self) -> AbrTicket {
        self.abr_ticket
    }
}

/// Fate of the outgoing source once this exact transition is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OutgoingDisposition {
    /// Keep the outgoing source available for the ordinary transition path.
    Retained,
    /// Stop relying on the outgoing source because it is no longer delivering.
    Abandoned,
}

/// Immutable route facts for one active-to-incoming transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(opt_in, with)]
pub struct VariantTransition {
    #[field(with(
        vis = "pub",
        doc = "Return this transition with an explicitly bound outgoing disposition."
    ))]
    outgoing_disposition: OutgoingDisposition,
    active_variant: VariantIndex,
    incoming_variant: VariantIndex,
    id: VariantTransitionId,
}

impl VariantTransition {
    /// Describe the exact source pair owned by one transition.
    #[must_use]
    pub const fn new(
        id: VariantTransitionId,
        active_variant: VariantIndex,
        incoming_variant: VariantIndex,
    ) -> Self {
        Self {
            active_variant,
            incoming_variant,
            id,
            outgoing_disposition: OutgoingDisposition::Retained,
        }
    }

    /// Variant that remains authoritative until promotion.
    #[must_use]
    pub const fn active_variant(self) -> VariantIndex {
        self.active_variant
    }

    /// Exact transition identity.
    #[must_use]
    pub const fn id(self) -> VariantTransitionId {
        self.id
    }

    /// Variant being prepared independently.
    #[must_use]
    pub const fn incoming_variant(self) -> VariantIndex {
        self.incoming_variant
    }

    /// Fate of the outgoing source for this exact transition.
    #[must_use]
    pub const fn outgoing_disposition(self) -> OutgoingDisposition {
        self.outgoing_disposition
    }
}
#[cfg(test)]
mod tests {
    use kithara_abr::{AbrMode, AbrReason, AbrState, PendingAbrDecision, VariantIndex};
    use kithara_test_utils::kithara;

    use super::*;

    fn ticket_for(target: usize) -> AbrTicket {
        let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        state.request_target(VariantIndex::new(target), AbrReason::UpSwitch);
        state
            .claim_pending_decision(VariantIndex::new(0))
            .map(PendingAbrDecision::ticket)
            .expect("requested target must produce an exact ticket")
    }

    #[kithara::test]
    fn transition_identity_includes_ticket_and_seek_epoch() {
        let state = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        state.request_target(VariantIndex::new(1), AbrReason::UpSwitch);
        let ticket = state
            .claim_pending_decision(VariantIndex::new(0))
            .expect("first target must have a ticket")
            .ticket();
        let first = VariantTransitionId::new(ticket);
        let same = VariantTransitionId::new(ticket);
        state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
        let after_seek = VariantTransitionId::new(
            state
                .claim_pending_decision(VariantIndex::new(0))
                .expect("superseding target must have a ticket")
                .ticket(),
        );

        assert_eq!(first, same);
        assert_ne!(first, after_seek);
    }

    #[kithara::test]
    fn transition_keeps_active_and_incoming_roles_distinct() {
        let ticket = ticket_for(1);
        let transition = VariantTransition::new(
            VariantTransitionId::new(ticket),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );

        assert_eq!(transition.active_variant(), VariantIndex::new(0));
        assert_eq!(transition.incoming_variant(), VariantIndex::new(1));
        assert_eq!(transition.id().abr_ticket(), ticket);
    }

    #[kithara::test]
    fn outgoing_disposition_defaults_to_retained_and_changes_immutably() {
        let retained = VariantTransition::new(
            VariantTransitionId::new(ticket_for(1)),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );

        let abandoned = retained.with_outgoing_disposition(OutgoingDisposition::Abandoned);

        assert_eq!(
            retained.outgoing_disposition(),
            OutgoingDisposition::Retained
        );
        assert_eq!(
            abandoned.outgoing_disposition(),
            OutgoingDisposition::Abandoned
        );
        assert_eq!(abandoned.id(), retained.id());
    }
}
