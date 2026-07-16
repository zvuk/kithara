use super::{arm::TempoArm, protocol::TempoParticipantStatus};
use crate::rt::{PresentationFrame, RenderFrame};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TempoParticipantObservation {
    effective_latency_frames: u32,
    max_raw_latency_frames: u32,
    membership_epoch: u64,
    participant_count: u8,
    presentation_boundary: Option<PresentationFrame>,
    render_boundary: Option<RenderFrame>,
    revision: u64,
    status: TempoParticipantStatus,
}

impl TempoParticipantObservation {
    pub(crate) const fn prepared(
        proposal: TempoArm,
        participant_count: u8,
        max_raw_latency_frames: u32,
        effective_latency_frames: u32,
        status: TempoParticipantStatus,
    ) -> Self {
        Self {
            effective_latency_frames,
            max_raw_latency_frames,
            membership_epoch: proposal.membership_epoch(),
            participant_count,
            presentation_boundary: Some(proposal.presentation_boundary()),
            render_boundary: Some(proposal.render_boundary()),
            revision: proposal.revision(),
            status,
        }
    }

    pub(crate) const fn terminal(
        revision: u64,
        membership_epoch: u64,
        participant_count: u8,
        status: TempoParticipantStatus,
    ) -> Self {
        Self {
            effective_latency_frames: 0,
            max_raw_latency_frames: 0,
            membership_epoch,
            participant_count,
            presentation_boundary: None,
            render_boundary: None,
            revision,
            status,
        }
    }

    pub(crate) const fn effective_latency_frames(self) -> u32 {
        self.effective_latency_frames
    }

    pub(crate) const fn max_raw_latency_frames(self) -> u32 {
        self.max_raw_latency_frames
    }

    pub(crate) const fn membership_epoch(self) -> u64 {
        self.membership_epoch
    }

    pub(crate) const fn participant_count(self) -> u8 {
        self.participant_count
    }

    pub(crate) const fn presentation_boundary(self) -> Option<PresentationFrame> {
        self.presentation_boundary
    }

    pub(crate) const fn render_boundary(self) -> Option<RenderFrame> {
        self.render_boundary
    }

    pub(crate) const fn revision(self) -> u64 {
        self.revision
    }

    pub(crate) const fn status(self) -> TempoParticipantStatus {
        self.status
    }
}
