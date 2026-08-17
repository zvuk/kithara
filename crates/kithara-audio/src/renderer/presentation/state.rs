use kithara_decode::PcmChunk;

use super::frontier::PresentationBarrier;
use crate::traits::{TempoBoundaryId, TempoDiscontinuityDebt};

pub(super) struct RawChunk {
    pub(super) chunk: PcmChunk,
    pub(super) consumed_frames: usize,
    pub(super) epoch: u64,
}

#[derive(Clone, Copy)]
pub(super) struct SourceEnd {
    pub(super) frame: u64,
    pub(super) rate: u32,
}

#[derive(Default)]
pub(super) struct SourceWindow {
    admitted: Option<SourceEnd>,
}

impl SourceWindow {
    pub(super) fn admit(&mut self, chunk: PcmChunk) -> PcmChunk {
        let rate = chunk.meta.spec.sample_rate.get();
        let start = self
            .admitted
            .filter(|admitted| admitted.rate == rate)
            .map_or(chunk.meta.frame_offset, |admitted| admitted.frame);
        self.admitted = Some(SourceEnd {
            frame: start.saturating_add(u64::from(chunk.meta.frames)),
            rate,
        });
        chunk
    }

    pub(super) fn emitted(&self, held_source_frames: u64) -> Option<SourceEnd> {
        self.admitted.map(|admitted| SourceEnd {
            frame: admitted.frame.saturating_sub(held_source_frames),
            ..admitted
        })
    }

    pub(super) fn clear(&mut self) {
        self.admitted = None;
    }
}

pub(super) enum RawItem {
    Barrier(PresentationBarrier),
    Data(RawChunk),
}

#[derive(Clone, Copy)]
pub(super) enum Terminal {
    Eof { epoch: u64 },
    Failed { epoch: u64 },
}

pub(super) struct Discontinuity {
    pub(super) barrier: Option<PresentationBarrier>,
    pub(super) boundary: TempoBoundaryId,
    pub(super) phase: DiscontinuityPhase,
}

pub(super) enum DiscontinuityPhase {
    Draining(TempoDiscontinuityDebt),
    Drained,
}
