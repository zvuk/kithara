use std::collections::VecDeque;

use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, PcmChunk, PcmSpec};

use super::{
    frontier::PresentationPublisher,
    output::{OutputBuffers, PresentedPcm},
    state::{Discontinuity, RawItem, SourceWindow, Terminal},
};
use crate::{
    pipeline::fetch::Fetch,
    runtime::StrictOutlet,
    traits::{AudioEffect, TempoEofDebt, TempoStage},
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Presentation {
    pub(super) buffer_error: Option<DecodeError>,
    pub(super) buffers: OutputBuffers,
    pub(super) effects: Vec<Box<dyn AudioEffect>>,
    pub(super) discontinuity: Option<Discontinuity>,
    pub(super) eof_debt: Option<TempoEofDebt>,
    #[field(get, vis = "pub(crate)", copy)]
    pub(super) epoch: u64,
    pub(super) failure_reset: bool,
    pub(super) final_committed: bool,
    pub(super) output: StrictOutlet<Fetch<PresentedPcm>>,
    pub(super) publisher: PresentationPublisher,
    pub(super) raw: VecDeque<RawItem>,
    pub(super) raw_admitted: usize,
    pub(super) raw_capacity: usize,
    pub(super) rejected_output: Option<PcmChunk>,
    pub(super) tempo: Option<Box<dyn TempoStage>>,
    pub(super) terminal: Option<Terminal>,
    #[field(get, vis = "pub(crate)", copy)]
    pub(super) terminal_sent: bool,
    pub(super) window: SourceWindow,
}

impl Presentation {
    pub(crate) fn new(
        raw_capacity: usize,
        chain: crate::pipeline::config::PresentationChain,
        pool: PcmPool,
        initial_spec: PcmSpec,
        output: StrictOutlet<Fetch<PresentedPcm>>,
        publisher: PresentationPublisher,
        epoch: u64,
    ) -> Self {
        let raw_capacity = raw_capacity.max(1);
        let mut buffers = OutputBuffers::new(pool);
        let buffer_error = buffers.prepare(initial_spec).err();
        Self {
            buffer_error,
            buffers,
            effects: chain.effects,
            discontinuity: None,
            eof_debt: None,
            epoch,
            failure_reset: false,
            final_committed: false,
            output,
            publisher,
            raw: VecDeque::with_capacity(raw_capacity),
            raw_admitted: 0,
            raw_capacity,
            rejected_output: None,
            tempo: chain.tempo,
            terminal: None,
            terminal_sent: false,
            window: SourceWindow::default(),
        }
    }

    pub(crate) fn is_raw_full(&self) -> bool {
        let has_capacity = self.has_raw_capacity();
        !has_capacity
    }

    pub(crate) const fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    pub(crate) fn preload_ready(&self, target: usize) -> bool {
        let raw_ready = self.raw_ready_for_preload(target);
        raw_ready && (self.final_committed || self.terminal_sent)
    }

    pub(crate) fn raw_ready_for_preload(&self, target: usize) -> bool {
        self.raw_admitted >= target || self.terminal.is_some()
    }

    pub(crate) const fn terminal_failed(&self) -> bool {
        matches!(self.terminal, Some(Terminal::Failed { .. }))
    }
}
