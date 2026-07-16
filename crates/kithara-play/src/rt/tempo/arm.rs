use crate::rt::{PresentationFrame, RenderFrame};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TempoArm {
    membership_epoch: u64,
    presentation_boundary: PresentationFrame,
    render_boundary: RenderFrame,
    revision: u64,
}

impl TempoArm {
    pub(crate) const fn new(
        revision: u64,
        membership_epoch: u64,
        render_boundary: RenderFrame,
        presentation_boundary: PresentationFrame,
    ) -> Self {
        Self {
            membership_epoch,
            presentation_boundary,
            render_boundary,
            revision,
        }
    }

    pub(crate) const fn membership_epoch(self) -> u64 {
        self.membership_epoch
    }

    pub(crate) const fn presentation_boundary(self) -> PresentationFrame {
        self.presentation_boundary
    }

    pub(crate) const fn render_boundary(self) -> RenderFrame {
        self.render_boundary
    }

    pub(crate) const fn revision(self) -> u64 {
        self.revision
    }
}
