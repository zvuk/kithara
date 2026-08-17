use kithara_decode::{DecodeError, DecodeResult, PcmChunk, PcmSpec};

use super::{
    coordinator::Presentation,
    frontier::PresentationBarrier,
    state::{RawChunk, RawItem},
};
use crate::{pipeline::fetch::Fetch, traits::TempoStage};

impl Presentation {
    #[must_use]
    pub(crate) fn admit(&mut self, fetch: Fetch<PcmChunk>) -> Option<Fetch<PcmChunk>> {
        if self.terminal.is_some() || !self.has_raw_capacity() {
            return Some(fetch);
        }
        let Fetch::Data { data, epoch } = fetch else {
            return Some(fetch);
        };
        let Some(raw_admitted) = self.raw_admitted.checked_add(1) else {
            return Some(Fetch::data(data, epoch));
        };
        self.raw.push_back(RawItem::Data(RawChunk {
            chunk: data,
            consumed_frames: 0,
            epoch,
        }));
        self.raw_admitted = raw_admitted;
        None
    }

    pub(crate) fn admit_barrier(
        &mut self,
        barrier: PresentationBarrier,
    ) -> Result<(), PresentationBarrier> {
        if barrier.epoch() != self.epoch || self.terminal.is_some() || !self.has_raw_capacity() {
            return Err(barrier);
        }
        self.raw.push_back(RawItem::Barrier(barrier));
        Ok(())
    }

    fn charged_source_quanta(&self) -> Option<usize> {
        let tempo = self
            .tempo
            .as_ref()
            .map_or(0, |tempo| tempo.buffered_source_quanta());
        self.raw
            .len()
            .checked_add(tempo)?
            .checked_add(usize::from(self.discontinuity.is_some()))
    }

    pub(super) fn has_raw_capacity(&self) -> bool {
        matches!(self.charged_source_quanta(), Some(charged) if charged < self.raw_capacity)
    }

    pub(super) fn checked_tempo_quanta(tempo: &dyn TempoStage) -> DecodeResult<usize> {
        let buffered = tempo.buffered_source_quanta();
        if buffered > 1 {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage buffered more than one source chunk",
            });
        }
        Ok(buffered)
    }

    pub(super) fn pending_buffer_spec(&self) -> Option<PcmSpec> {
        if let Some(PresentationBarrier::DecoderReplaced { spec, .. }) = self
            .discontinuity
            .as_ref()
            .and_then(|discontinuity| discontinuity.barrier)
        {
            return Some(spec);
        }
        if let Some(RawItem::Barrier(PresentationBarrier::DecoderReplaced { spec, .. })) =
            self.raw.front()
        {
            return Some(*spec);
        }
        None
    }

    pub(super) fn take_source_chunk<F>(&mut self, retire: &mut F) -> DecodeResult<PcmChunk>
    where
        F: FnMut(PcmChunk),
    {
        let Some(RawItem::Data(raw)) = self.raw.pop_front() else {
            return Err(DecodeError::InvalidData {
                detail: "presentation raw queue lost its tempo source",
            });
        };
        if raw.consumed_frames != 0 || raw.chunk.frames() == 0 {
            retire(raw.chunk);
            return Err(DecodeError::InvalidData {
                detail: "presentation received an invalid tempo source chunk",
            });
        }
        Ok(raw.chunk)
    }
}
