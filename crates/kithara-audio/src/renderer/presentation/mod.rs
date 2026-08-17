mod boundary;
mod commit;
mod coordinator;
mod frontier;
mod intake;
mod lifecycle;
mod output;
mod render;
mod state;

pub(crate) use coordinator::Presentation;
pub(crate) use frontier::{
    PresentationBarrier, PresentationFrontier, PresentationPublisher, presentation_cell,
};
pub(crate) use output::{
    OutputDisposition, PRESENTATION_FRAMES, PRESENTATION_RING_BLOCKS, PresentResult,
    PresentedBlock, PresentedPcm,
};

#[cfg(test)]
mod tests;
