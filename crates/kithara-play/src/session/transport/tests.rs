use super::{
    barrier::{collect_armed, collect_readiness},
    control::start_preparation,
};
use crate::{
    api::{SlotId, Tempo, TransportPreparationFailure},
    rt::{
        PresentationFrame, RenderFrame, SessionTransportCommit, TempoArm,
        TempoParticipantObservation, TempoParticipantStatus,
    },
    session::{
        protocol::SessionError,
        state::{
            PendingTransportCommit, SessionTransportState, TransportBoundary,
            TransportParticipantId, TransportParticipantLedger, TransportParticipantReady,
            TransportPreparation,
        },
    },
};

fn commit(tempo: f64, revision: u64) -> SessionTransportCommit {
    SessionTransportCommit::new(Tempo::new(tempo).expect("valid tempo"), true, revision)
}

fn participant(player_id: u64, slot: u64) -> TransportParticipantLedger {
    TransportParticipantLedger {
        id: TransportParticipantId {
            player_id,
            slot_id: SlotId::new(slot),
        },
        ready: None,
    }
}

fn observation(revision: u64, status: TempoParticipantStatus) -> TempoParticipantObservation {
    observation_with(revision, 3, 1, status)
}

fn observation_with(
    revision: u64,
    membership_epoch: u64,
    participant_count: u8,
    status: TempoParticipantStatus,
) -> TempoParticipantObservation {
    if matches!(
        status,
        TempoParticipantStatus::Aborted
            | TempoParticipantStatus::Applied
            | TempoParticipantStatus::Rejected(_)
    ) {
        return TempoParticipantObservation::terminal(
            revision,
            membership_epoch,
            participant_count,
            status,
        );
    }
    TempoParticipantObservation::prepared(
        TempoArm::new(
            revision,
            membership_epoch,
            RenderFrame::new(4096),
            PresentationFrame::new(4096),
        ),
        participant_count,
        2880,
        0,
        status,
    )
}

#[test]
fn mixed_ready_and_rejected_participants_reject_the_transaction() {
    let mut preparation = TransportPreparation {
        boundary: None,
        candidate: commit(100.0, 2),
        participants: vec![participant(1, 1), participant(2, 1)],
    };
    let observations = [
        (
            preparation.participants[0].id,
            observation(2, TempoParticipantStatus::Ready),
        ),
        (
            preparation.participants[1].id,
            observation(
                2,
                TempoParticipantStatus::Rejected(TransportPreparationFailure::UnsupportedRate),
            ),
        ),
    ];

    assert!(matches!(
        collect_readiness(&mut preparation, &observations),
        Err(SessionError::TransportPreparationRejected {
            player_id: 2,
            revision: 2,
            slot,
            reason: TransportPreparationFailure::UnsupportedRate,
        }) if slot == SlotId::new(1)
    ));
    assert_eq!(preparation.boundary, None);
}

#[test]
fn all_ready_then_all_armed_complete_one_exact_barrier() {
    let mut preparation = TransportPreparation {
        boundary: None,
        candidate: commit(100.0, 2),
        participants: vec![participant(1, 1), participant(2, 1)],
    };
    let ready = preparation
        .participants
        .iter()
        .map(|participant| {
            (
                participant.id,
                observation(2, TempoParticipantStatus::Ready),
            )
        })
        .collect::<Vec<_>>();

    let boundary = collect_readiness(&mut preparation, &ready)
        .expect("all readiness observations are valid")
        .expect("all participants are ready");
    assert_eq!(boundary.render, RenderFrame::new(4096));

    let armed = preparation
        .participants
        .iter()
        .map(|participant| {
            (
                participant.id,
                observation(2, TempoParticipantStatus::Armed),
            )
        })
        .collect::<Vec<_>>();
    assert!(collect_armed(&preparation, &armed).expect("armed barrier is valid"));
}

#[test]
fn armed_epoch_mismatch_rejects_the_barrier() {
    let boundary = TransportBoundary {
        presentation: PresentationFrame::new(4096),
        render: RenderFrame::new(4096),
    };
    let mut participant = participant(1, 1);
    participant.ready = Some(TransportParticipantReady {
        boundary,
        effective_latency_frames: 0,
        max_raw_latency_frames: 2880,
        membership_epoch: 3,
        participant_count: 1,
    });
    let preparation = TransportPreparation {
        boundary: Some(boundary),
        candidate: commit(100.0, 2),
        participants: vec![participant],
    };
    let observations = [(
        preparation.participants[0].id,
        observation_with(2, 4, 1, TempoParticipantStatus::Armed),
    )];

    assert!(matches!(
        collect_armed(&preparation, &observations),
        Err(SessionError::TransportPreparationRejected {
            revision: 2,
            reason: TransportPreparationFailure::MembershipChanged,
            ..
        })
    ));
}

#[test]
fn older_ready_revision_is_ignored_until_the_candidate_is_observed() {
    let mut preparation = TransportPreparation {
        boundary: None,
        candidate: commit(100.0, 2),
        participants: vec![participant(1, 1)],
    };
    let observations = [(
        preparation.participants[0].id,
        observation(1, TempoParticipantStatus::Ready),
    )];

    assert_eq!(
        collect_readiness(&mut preparation, &observations)
            .expect("an older observation is harmless"),
        None
    );
    assert!(preparation.participants[0].ready.is_none());
}

#[test]
fn future_ready_revision_rejects_the_transaction() {
    let mut preparation = TransportPreparation {
        boundary: None,
        candidate: commit(100.0, 2),
        participants: vec![participant(1, 1)],
    };
    let observations = [(
        preparation.participants[0].id,
        observation(3, TempoParticipantStatus::Ready),
    )];

    assert!(matches!(
        collect_readiness(&mut preparation, &observations),
        Err(SessionError::TransportPreparationRejected {
            revision: 2,
            reason: TransportPreparationFailure::StaleRevision,
            ..
        })
    ));
}

#[test]
fn preparation_burns_revision_without_publishing_candidate() {
    let active = commit(120.0, 1);
    let candidate = commit(100.0, 2);
    let mut transport = SessionTransportState {
        accepted: Some(active),
        last_revision: 1,
        observed: Some(active),
        ..SessionTransportState::default()
    };

    start_preparation(&mut transport, candidate, Vec::new());

    assert_eq!(transport.last_revision, 2);
    assert_eq!(transport.accepted, Some(active));
    assert_eq!(transport.observed, Some(active));
    assert!(matches!(
        transport.pending,
        Some(PendingTransportCommit::Preparing(_))
    ));
}
