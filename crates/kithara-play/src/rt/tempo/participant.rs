use num_traits::ToPrimitive;
use smallvec::SmallVec;

use super::{
    TempoArm, TempoParticipantCommand, TempoParticipantEndpoint, TempoParticipantObservation,
    TempoParticipantStatus, TempoStage,
};
#[cfg(not(target_arch = "wasm32"))]
use crate::rt::track::{ElasticPlanError, ElasticRenderError, ElasticTempoPreparationOutcome};
use crate::{
    api::{SessionBeat, TransportPreparationFailure},
    rt::{
        PresentationFrame, RenderFrame, StreamShape,
        context::{RenderContext, SessionTransportCommit},
        track::PlayerTrack,
    },
};

const CANDIDATE_HORIZON_BLOCKS: i64 = 2;
const PREPARE_LEAD_BLOCKS: i64 = 4;

pub(crate) struct TempoParticipantState {
    abort_revision: Option<u64>,
    arm: Option<TempoArm>,
    arm_request: Option<TempoArm>,
    endpoint: Option<TempoParticipantEndpoint>,
    frozen: bool,
    membership_epoch: u64,
    proposal: Option<RenderFrame>,
    stage: Option<TempoStage>,
    terminal: Option<(u64, TempoParticipantStatus)>,
}

impl TempoParticipantState {
    pub(crate) const fn new(endpoint: Option<TempoParticipantEndpoint>) -> Self {
        Self {
            abort_revision: None,
            arm: None,
            arm_request: None,
            endpoint,
            frozen: false,
            membership_epoch: 0,
            proposal: None,
            stage: None,
            terminal: None,
        }
    }

    pub(crate) const fn frozen(&self) -> bool {
        self.frozen || self.abort_revision.is_some()
    }

    pub(crate) fn membership_changed(&mut self) {
        self.membership_epoch = self.membership_epoch.saturating_add(1);
    }

    pub(crate) fn receive_commands(&mut self) {
        let Some(endpoint) = self.endpoint.as_mut() else {
            return;
        };
        while let Some(command) = endpoint.receive() {
            match command {
                TempoParticipantCommand::Abort(revision) => {
                    if self.stage.is_some_and(|stage| stage.revision() == revision) {
                        self.abort_revision = Some(revision);
                        self.arm = None;
                        self.arm_request = None;
                        self.frozen = false;
                        self.proposal = None;
                        self.stage = None;
                    }
                }
                TempoParticipantCommand::Arm(arm) => {
                    self.arm_request = Some(arm);
                }
                TempoParticipantCommand::Stage(stage) => {
                    if self.abort_revision.is_none()
                        && let Some(displaced) = self
                            .stage
                            .filter(|current| current.revision() != stage.revision())
                    {
                        self.abort_revision = Some(displaced.revision());
                    }
                    self.arm = None;
                    self.arm_request = None;
                    self.frozen = false;
                    self.proposal = None;
                    self.stage = Some(stage);
                    self.terminal = None;
                }
            }
        }
    }

    pub(crate) fn process<'a>(
        &mut self,
        tracks: impl Iterator<Item = &'a mut PlayerTrack>,
        context: &RenderContext,
    ) {
        let mut tracks: SmallVec<
            [&'a mut PlayerTrack; crate::rt::processor::PlayerNodeProcessor::MAX_TRACKS],
        > = tracks.filter(|track| track.binding().is_some()).collect();
        if let Some(revision) = self.abort_revision.take() {
            Self::abort_tracks(&mut tracks, revision);
            self.publish_terminal(revision, TempoParticipantStatus::Aborted);
            return;
        }
        let Some(stage) = self.stage else {
            if let Some((revision, status)) = self.terminal {
                self.publish(TempoParticipantObservation::terminal(
                    revision,
                    self.membership_epoch,
                    u8::try_from(tracks.len()).unwrap_or(u8::MAX),
                    status,
                ));
            }
            return;
        };
        let Some(active) = context.transport_commit() else {
            self.reject(
                &mut tracks,
                stage.revision(),
                TransportPreparationFailure::StaleRevision,
            );
            return;
        };
        if active.revision() == stage.revision() {
            Self::apply_tracks(&mut tracks, stage.revision());
            self.stage = None;
            self.arm = None;
            self.frozen = false;
            self.proposal = None;
            self.publish_terminal(stage.revision(), TempoParticipantStatus::Applied);
            return;
        }
        if Some(active.revision()) != stage.previous_revision() {
            self.reject(
                &mut tracks,
                stage.revision(),
                TransportPreparationFailure::StaleRevision,
            );
            return;
        }
        if let Some(arm) = self.arm_request.take() {
            if arm.revision() != stage.revision() {
                self.reject(
                    &mut tracks,
                    stage.revision(),
                    TransportPreparationFailure::StaleRevision,
                );
                return;
            }
            if arm.membership_epoch() != self.membership_epoch {
                self.reject(
                    &mut tracks,
                    stage.revision(),
                    TransportPreparationFailure::MembershipChanged,
                );
                return;
            }
            if self.proposal != Some(arm.render_boundary()) {
                self.reject(
                    &mut tracks,
                    stage.revision(),
                    TransportPreparationFailure::StaleRevision,
                );
                return;
            }
            self.arm = Some(arm);
            self.frozen = true;
        }
        let Some(render_boundary) = self.render_boundary(context, stage.shape()) else {
            self.reject(
                &mut tracks,
                stage.revision(),
                TransportPreparationFailure::BoundaryExpired,
            );
            return;
        };
        if render_boundary <= context.render_frames().end {
            self.reject(
                &mut tracks,
                stage.revision(),
                TransportPreparationFailure::BoundaryExpired,
            );
            return;
        }
        let Some(candidate) = candidate_context(context, stage, render_boundary) else {
            self.reject(
                &mut tracks,
                stage.revision(),
                TransportPreparationFailure::BindingUnavailable,
            );
            return;
        };
        let (pending, max_raw_latency_frames) =
            match Self::prepare_tracks(&mut tracks, context, &candidate, stage.revision()) {
                Ok(preparation) => preparation,
                Err(failure) => {
                    self.reject(&mut tracks, stage.revision(), failure);
                    return;
                }
            };
        let presentation_boundary = PresentationFrame::new(render_boundary.get());
        if let Some(arm) = self.arm
            && arm.presentation_boundary() != presentation_boundary
        {
            self.reject(
                &mut tracks,
                stage.revision(),
                TransportPreparationFailure::StaleRevision,
            );
            return;
        }
        let status = if pending {
            TempoParticipantStatus::Preparing
        } else if self.arm.is_some() {
            TempoParticipantStatus::Armed
        } else {
            TempoParticipantStatus::Ready
        };
        self.publish(TempoParticipantObservation::prepared(
            TempoArm::new(
                stage.revision(),
                self.membership_epoch,
                render_boundary,
                presentation_boundary,
            ),
            u8::try_from(tracks.len()).unwrap_or(u8::MAX),
            max_raw_latency_frames,
            0,
            status,
        ));
    }

    fn render_boundary(
        &mut self,
        context: &RenderContext,
        shape: StreamShape,
    ) -> Option<RenderFrame> {
        if let Some(arm) = self.arm {
            return Some(arm.render_boundary());
        }
        if let Some(proposal) = self.proposal {
            return Some(proposal);
        }
        let proposal = proposal_boundary(context, shape)?;
        self.proposal = Some(proposal);
        Some(proposal)
    }

    fn prepare_tracks(
        tracks: &mut [&mut PlayerTrack],
        current: &RenderContext,
        candidate: &RenderContext,
        revision: u64,
    ) -> Result<(bool, u32), TransportPreparationFailure> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut pending = false;
            let mut max_raw_latency_frames = 0;
            for track in tracks {
                match track.prepare_tempo(current, candidate, revision) {
                    Ok(ElasticTempoPreparationOutcome::Pending) => pending = true,
                    Ok(ElasticTempoPreparationOutcome::Ready { raw_latency_frames }) => {
                        max_raw_latency_frames = max_raw_latency_frames.max(raw_latency_frames);
                    }
                    Err(error) => return Err(map_prepare_error(&error)),
                }
            }
            Ok((pending, max_raw_latency_frames))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (tracks, current, candidate, revision);
            Err(TransportPreparationFailure::RendererUnavailable)
        }
    }

    fn reject(
        &mut self,
        tracks: &mut [&mut PlayerTrack],
        revision: u64,
        failure: TransportPreparationFailure,
    ) {
        Self::abort_tracks(tracks, revision);
        self.stage = None;
        self.arm = None;
        self.arm_request = None;
        self.frozen = false;
        self.proposal = None;
        self.publish_terminal(revision, TempoParticipantStatus::Rejected(failure));
    }

    fn abort_tracks(tracks: &mut [&mut PlayerTrack], revision: u64) {
        #[cfg(not(target_arch = "wasm32"))]
        for track in tracks {
            track.abort_tempo(revision);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (tracks, revision);
    }

    fn apply_tracks(tracks: &mut [&mut PlayerTrack], revision: u64) {
        #[cfg(not(target_arch = "wasm32"))]
        for track in tracks {
            track.apply_tempo(revision);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (tracks, revision);
    }

    fn publish_terminal(&mut self, revision: u64, status: TempoParticipantStatus) {
        self.terminal = Some((revision, status));
        self.publish(TempoParticipantObservation::terminal(
            revision,
            self.membership_epoch,
            0,
            status,
        ));
    }

    fn publish(&mut self, observation: TempoParticipantObservation) {
        if let Some(endpoint) = self.endpoint.as_mut() {
            endpoint.publish(observation);
        }
    }

    #[cfg(test)]
    pub(super) const fn proposal(&self) -> Option<RenderFrame> {
        self.proposal
    }
}

pub(super) fn proposal_boundary(
    context: &RenderContext,
    shape: StreamShape,
) -> Option<RenderFrame> {
    let lead = i64::from(shape.max_block_frames.get()).checked_mul(PREPARE_LEAD_BLOCKS)?;
    Some(RenderFrame::new(
        context.render_frames().end.get().checked_add(lead)?,
    ))
}

pub(super) fn candidate_context(
    current: &RenderContext,
    stage: TempoStage,
    boundary: RenderFrame,
) -> Option<RenderContext> {
    if current.sample_rate() != stage.shape().sample_rate {
        return None;
    }
    let active = current.transport_commit()?;
    let beats = current.session_beats()?;
    let lead_frames = boundary
        .get()
        .checked_sub(current.render_frames().end.get())?;
    if lead_frames < 0 {
        return None;
    }
    let sample_rate = f64::from(stage.shape().sample_rate.get());
    let lead_frames = lead_frames.to_f64()?;
    let boundary_beat =
        beats.end.get() + lead_frames * active.tempo().beats_per_minute() / (sample_rate * 60.0);
    let output_frames =
        i64::from(stage.shape().max_block_frames.get()).checked_mul(CANDIDATE_HORIZON_BLOCKS)?;
    let end_frame = boundary.get().checked_add(output_frames)?;
    let output_frames_float = output_frames.to_f64()?;
    let end_beat = boundary_beat
        + output_frames_float * stage.tempo().beats_per_minute() / (sample_rate * 60.0);
    RenderContext::new(
        boundary..RenderFrame::new(end_frame),
        stage.shape().sample_rate,
        Some(SessionBeat::new(boundary_beat).ok()?..SessionBeat::new(end_beat).ok()?),
        Some(SessionTransportCommit::new(
            stage.tempo(),
            stage.is_playing(),
            stage.revision(),
        )),
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn map_prepare_error(error: &ElasticRenderError) -> TransportPreparationFailure {
    match error {
        ElasticRenderError::Plan(ElasticPlanError::UnsupportedRate { .. }) => {
            TransportPreparationFailure::UnsupportedRate
        }
        ElasticRenderError::Plan(
            ElasticPlanError::Binding(_)
            | ElasticPlanError::OutsideMarkerDomain { .. }
            | ElasticPlanError::SessionBeatsUnavailable,
        ) => TransportPreparationFailure::BindingUnavailable,
        ElasticRenderError::FetchWindowMismatch
        | ElasticRenderError::SourceWindowDeadlineMissed => {
            TransportPreparationFailure::LookAheadUnavailable
        }
        _ => TransportPreparationFailure::RendererUnavailable,
    }
}
