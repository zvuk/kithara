use kithara_decode::{DecodeResult, PcmChunk};

use super::{
    coordinator::Presentation,
    state::{DiscontinuityPhase, RawItem, Terminal},
};
use crate::traits::TempoPrepareRequest;

impl Presentation {
    pub(crate) fn service_off_rt(&mut self) {
        if let Some(tempo) = &mut self.tempo {
            tempo.release_retired_off_rt();
        }
        if self.buffer_error.is_some()
            || self.terminal_sent
            || matches!(self.terminal, Some(Terminal::Failed { .. }))
        {
            return;
        }
        let pending = self.pending_buffer_spec();
        let tempo_discontinuity_drained = matches!(
            self.discontinuity
                .as_ref()
                .map(|discontinuity| &discontinuity.phase),
            Some(DiscontinuityPhase::Drained)
        );
        let prepare_pending_buffers = self.tempo.is_none() || tempo_discontinuity_drained;
        if let Some(spec) = pending
            && prepare_pending_buffers
            && let Err(error) = self.buffers.prepare(spec)
        {
            self.buffer_error = Some(error);
            return;
        }
        if self.discontinuity.is_some() {
            return;
        }
        let Some(tempo) = &mut self.tempo else {
            return;
        };
        let request = pending.map_or_else(
            || TempoPrepareRequest::Current {
                spec: tempo.output_spec(),
            },
            |spec| TempoPrepareRequest::DecoderBoundary { spec },
        );
        if let Err(error) = tempo.service_off_rt(request) {
            self.buffer_error = Some(error);
        }
    }

    pub(crate) fn release_retired_off_rt(&mut self) {
        if let Some(tempo) = &mut self.tempo {
            tempo.release_retired_off_rt();
        }
    }

    pub(crate) fn release_rejected_off_rt(&mut self) {
        drop(self.rejected_output.take());
    }

    pub(crate) fn abort_failed<F>(&mut self, epoch: u64, mut retire: F) -> DecodeResult<()>
    where
        F: FnMut(PcmChunk),
    {
        self.retire_raw(&mut retire);
        self.deactivate_chain(&mut retire)?;
        self.window.clear();
        self.discontinuity = None;
        self.eof_debt = None;
        self.failure_reset = true;
        self.terminal = Some(Terminal::Failed { epoch });
        self.terminal_sent = false;
        Ok(())
    }

    pub(crate) fn finish_eof(&mut self, epoch: u64) {
        self.eof_debt = None;
        self.failure_reset = false;
        self.terminal = Some(Terminal::Eof { epoch });
        self.terminal_sent = false;
    }

    pub(crate) fn finish_failed(&mut self, epoch: u64) {
        self.eof_debt = None;
        self.failure_reset = false;
        self.terminal = Some(Terminal::Failed { epoch });
        self.terminal_sent = false;
    }

    pub(crate) fn flush_wake_signals(&self) {
        self.output.flush_wake_signals();
    }

    pub(crate) fn reset_epoch<F>(&mut self, epoch: u64, mut retire: F) -> DecodeResult<()>
    where
        F: FnMut(PcmChunk),
    {
        if self.epoch == epoch {
            return Ok(());
        }
        self.retire_raw(&mut retire);
        self.deactivate_chain(&mut retire)?;
        self.window.clear();
        self.discontinuity = None;
        self.eof_debt = None;
        self.epoch = epoch;
        self.failure_reset = false;
        self.final_committed = false;
        self.raw_admitted = 0;
        self.terminal = None;
        self.terminal_sent = false;
        self.publisher.reset();
        Ok(())
    }

    pub(super) fn deactivate_chain<F>(&mut self, retire: &mut F) -> DecodeResult<()>
    where
        F: FnMut(PcmChunk),
    {
        if let Some(tempo) = &mut self.tempo {
            tempo.deactivate(retire)?;
        }
        self.reset_effects();
        Ok(())
    }

    pub(super) fn reset_effects(&mut self) {
        for effect in &mut self.effects {
            effect.reset();
        }
    }

    fn retire_raw<F>(&mut self, retire: &mut F)
    where
        F: FnMut(PcmChunk),
    {
        let data = self.raw.drain(..).filter_map(|item| match item {
            RawItem::Data(raw) => Some(raw.chunk),
            RawItem::Barrier(_) => None,
        });
        for chunk in data {
            retire(chunk);
        }
    }
}
