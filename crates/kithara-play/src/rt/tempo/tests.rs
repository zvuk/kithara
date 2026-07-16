use std::num::NonZeroU32;

use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{
    TempoArm, TempoParticipantCommand, TempoParticipantObservation, TempoParticipantState,
    TempoParticipantStatus, TempoStage,
    participant::{candidate_context, proposal_boundary},
    tempo_participant_channel,
};
use crate::{
    api::{SessionBeat, Tempo, TransportPreparationFailure},
    rt::{
        PresentationFrame, RenderFrame, StreamShape,
        context::{RenderContext, SessionTransportCommit},
        track::PlayerTrack,
    },
};

fn stage() -> TempoStage {
    TempoStage::new(
        7,
        Some(6),
        Tempo::new(100.0).expect("valid tempo"),
        true,
        StreamShape::new(
            NonZeroU32::new(48_000).expect("static sample rate"),
            NonZeroU32::new(512).expect("static block size"),
        ),
    )
}

fn context(tempo: f64, revision: u64) -> RenderContext {
    context_at(0, tempo, revision)
}

fn context_at(start: i64, tempo: f64, revision: u64) -> RenderContext {
    let sample_rate = NonZeroU32::new(48_000).expect("static sample rate");
    let beat_start = start.to_f64().expect("test frame is representable") * tempo
        / (f64::from(sample_rate.get()) * 60.0);
    let beat_end = (start + 512).to_f64().expect("test frame is representable") * tempo
        / (f64::from(sample_rate.get()) * 60.0);
    RenderContext::new(
        RenderFrame::new(start)..RenderFrame::new(start + 512),
        sample_rate,
        Some(
            SessionBeat::new(beat_start).expect("valid beat")
                ..SessionBeat::new(beat_end).expect("valid beat"),
        ),
        Some(SessionTransportCommit::new(
            Tempo::new(tempo).expect("valid tempo"),
            true,
            revision,
        )),
    )
    .expect("valid render context")
}

#[kithara::test]
fn control_and_endpoint_exchange_revisioned_commands_and_observations() {
    let (mut endpoint, mut control) = tempo_participant_channel();
    let stage = stage();
    control.stage(stage).expect("stage lane has capacity");
    assert_eq!(
        endpoint.receive(),
        Some(TempoParticipantCommand::Stage(stage))
    );

    let observation = TempoParticipantObservation::prepared(
        TempoArm::new(
            stage.revision(),
            3,
            RenderFrame::new(4096),
            PresentationFrame::new(4096),
        ),
        2,
        2880,
        0,
        TempoParticipantStatus::Ready,
    );
    endpoint.publish(observation);
    assert_eq!(control.observation(), observation);
}

#[kithara::test]
fn candidate_context_uses_active_tempo_before_boundary_and_candidate_after_it() {
    let active = context(120.0, 6);
    let stage = stage();
    let boundary = proposal_boundary(&active, stage.shape()).expect("proposal boundary");
    let candidate = candidate_context(&active, stage, boundary).expect("candidate context");

    assert_eq!(boundary, RenderFrame::new(2_560));
    assert_eq!(
        candidate.render_frames(),
        &(RenderFrame::new(2_560)..RenderFrame::new(3_584))
    );
    let beats = candidate.session_beats().expect("candidate beat range");
    let expected_boundary = 2_560.0 * 120.0 / (48_000.0 * 60.0);
    let expected_end = expected_boundary + 1_024.0 * 100.0 / (48_000.0 * 60.0);
    assert!((beats.start.get() - expected_boundary).abs() < f64::EPSILON);
    assert!((beats.end.get() - expected_end).abs() < f64::EPSILON);
}

#[kithara::test]
fn participant_rejects_membership_change_before_arm() {
    let (endpoint, mut control) = tempo_participant_channel();
    let mut participant = TempoParticipantState::new(Some(endpoint));
    let stage = stage();
    control.stage(stage).expect("stage lane has capacity");
    participant.receive_commands();
    participant.process(std::iter::empty::<&mut PlayerTrack>(), &context(120.0, 6));
    let ready = control.observation();
    assert_eq!(ready.status(), TempoParticipantStatus::Ready);

    participant.membership_changed();
    control
        .arm(TempoArm::new(
            stage.revision(),
            ready.membership_epoch(),
            ready.render_boundary().expect("ready render boundary"),
            ready
                .presentation_boundary()
                .expect("ready presentation boundary"),
        ))
        .expect("arm lane has capacity");
    participant.receive_commands();
    participant.process(std::iter::empty::<&mut PlayerTrack>(), &context(120.0, 6));

    assert_eq!(
        control.observation().status(),
        TempoParticipantStatus::Rejected(TransportPreparationFailure::MembershipChanged)
    );
    assert!(!participant.frozen());
}

#[kithara::test]
fn participant_keeps_one_proposed_boundary_until_arm() {
    let (endpoint, mut control) = tempo_participant_channel();
    let mut participant = TempoParticipantState::new(Some(endpoint));
    control.stage(stage()).expect("stage lane has capacity");
    participant.receive_commands();

    participant.process(
        std::iter::empty::<&mut PlayerTrack>(),
        &context_at(0, 120.0, 6),
    );
    let first = control.observation();
    assert_eq!(first.status(), TempoParticipantStatus::Ready);

    participant.process(
        std::iter::empty::<&mut PlayerTrack>(),
        &context_at(512, 120.0, 6),
    );
    let second = control.observation();
    assert_eq!(second.status(), TempoParticipantStatus::Ready);
    assert_eq!(second.render_boundary(), first.render_boundary());
    assert_eq!(
        second.presentation_boundary(),
        first.presentation_boundary()
    );
}

#[kithara::test]
fn participant_rejects_an_expired_proposed_boundary() {
    let (endpoint, mut control) = tempo_participant_channel();
    let mut participant = TempoParticipantState::new(Some(endpoint));
    control.stage(stage()).expect("stage lane has capacity");
    participant.receive_commands();
    participant.process(
        std::iter::empty::<&mut PlayerTrack>(),
        &context_at(0, 120.0, 6),
    );
    assert_eq!(
        control.observation().status(),
        TempoParticipantStatus::Ready
    );

    participant.process(
        std::iter::empty::<&mut PlayerTrack>(),
        &context_at(2_560, 120.0, 6),
    );

    assert_eq!(
        control.observation().status(),
        TempoParticipantStatus::Rejected(TransportPreparationFailure::BoundaryExpired)
    );
    assert!(!participant.frozen());
    assert!(participant.proposal().is_none());
}

#[kithara::test]
fn participant_finishes_abort_before_preparing_a_replacement_stage() {
    let (endpoint, mut control) = tempo_participant_channel();
    let mut participant = TempoParticipantState::new(Some(endpoint));
    let first = stage();
    let replacement = TempoStage::new(
        8,
        Some(6),
        Tempo::new(90.0).expect("valid tempo"),
        true,
        first.shape(),
    );
    control.stage(first).expect("stage lane has capacity");
    participant.receive_commands();
    participant.process(std::iter::empty::<&mut PlayerTrack>(), &context(120.0, 6));

    control
        .abort(first.revision())
        .expect("abort lane has capacity");
    control
        .stage(replacement)
        .expect("replacement stage lane has capacity");
    participant.receive_commands();
    assert!(participant.frozen());
    participant.process(std::iter::empty::<&mut PlayerTrack>(), &context(120.0, 6));
    assert_eq!(
        control.observation().status(),
        TempoParticipantStatus::Aborted
    );

    participant.process(std::iter::empty::<&mut PlayerTrack>(), &context(120.0, 6));
    let replacement_ready = control.observation();
    assert_eq!(replacement_ready.revision(), replacement.revision());
    assert_eq!(replacement_ready.status(), TempoParticipantStatus::Ready);
}

#[kithara::test]
fn participant_applies_only_after_the_shared_context_commits_the_revision() {
    let (endpoint, mut control) = tempo_participant_channel();
    let mut participant = TempoParticipantState::new(Some(endpoint));
    let stage = stage();
    control.stage(stage).expect("stage lane has capacity");
    participant.receive_commands();
    participant.process(std::iter::empty::<&mut PlayerTrack>(), &context(120.0, 6));
    let ready = control.observation();
    let render_boundary = ready.render_boundary().expect("ready render boundary");
    let presentation_boundary = ready
        .presentation_boundary()
        .expect("ready presentation boundary");
    assert_eq!(
        presentation_boundary,
        PresentationFrame::new(render_boundary.get())
    );
    assert_eq!(ready.effective_latency_frames(), 0);

    control
        .arm(TempoArm::new(
            stage.revision(),
            ready.membership_epoch(),
            render_boundary,
            presentation_boundary,
        ))
        .expect("arm lane has capacity");
    participant.receive_commands();
    participant.process(std::iter::empty::<&mut PlayerTrack>(), &context(120.0, 6));
    assert_eq!(
        control.observation().status(),
        TempoParticipantStatus::Armed
    );
    assert!(participant.frozen());

    participant.process(std::iter::empty::<&mut PlayerTrack>(), &context(100.0, 7));
    assert_eq!(
        control.observation().status(),
        TempoParticipantStatus::Applied
    );
    assert!(!participant.frozen());
}
