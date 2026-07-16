use std::num::{NonZeroU64, NonZeroUsize};

use kithara_bufpool::PcmBuf;
use kithara_decode::{PcmChunk, PcmSpec};

use super::{
    SourceAudioActivity, SourceAudioDemand, SourceAudioError, SourceFrameRange,
    model::{
        SourceAudioCaptureOutcome, SourceAudioCommand, SourceAudioPacket, SourceAudioRole,
        SourceAudioStatus, SourceAudioTerminal, SourceAudioWindow, sample_count,
    },
};
use crate::runtime::{Inlet, Outlet};

pub(crate) struct SourceAudioTap {
    lane_id: NonZeroU64,
    command_inlet: Inlet<SourceAudioCommand>,
    outputs: SourceAudioOutputs,
    trash_inlet: Inlet<SourceAudioWindow>,
    buffers: Vec<PcmBuf>,
    required_frames: NonZeroUsize,
    prepared_frames: usize,
    active_spec: Option<PcmSpec>,
    prepared_spec: Option<PcmSpec>,
    role: SourceAudioRole,
    demand: Option<SourceAudioDemand>,
    terminal_sent: Option<(u64, SourceAudioTerminal)>,
}

impl SourceAudioTap {
    pub(crate) fn new(
        lane_id: NonZeroU64,
        command_inlet: Inlet<SourceAudioCommand>,
        outputs: SourceAudioOutputs,
        trash_inlet: Inlet<SourceAudioWindow>,
        buffers: Vec<PcmBuf>,
        initial_frames: NonZeroUsize,
    ) -> Self {
        Self {
            lane_id,
            command_inlet,
            outputs,
            trash_inlet,
            buffers,
            required_frames: initial_frames,
            prepared_frames: 0,
            active_spec: None,
            prepared_spec: None,
            role: SourceAudioRole::Mirror,
            demand: None,
            terminal_sent: None,
        }
    }

    pub(crate) fn service(&mut self) -> Result<(), SourceAudioError> {
        self.reclaim_pending_data();
        while let Some(window) = self.trash_inlet.try_pop() {
            self.buffers.push(window.release_samples());
        }

        if !self.command_inlet.has_producer() {
            self.active_spec = None;
            self.demand = None;
            return Ok(());
        }

        while let Some(command) = self.command_inlet.try_pop() {
            match command {
                SourceAudioCommand::Activate {
                    lane_id,
                    role,
                    spec,
                } if lane_id == self.lane_id => {
                    self.active_spec = Some(spec);
                    self.role = role;
                    self.demand = None;
                    self.terminal_sent = None;
                }
                SourceAudioCommand::Deactivate { lane_id } if lane_id == self.lane_id => {
                    self.active_spec = None;
                    self.demand = None;
                }
                SourceAudioCommand::Demand(demand)
                    if demand.lane_id == self.lane_id && self.active_spec.is_some() =>
                {
                    self.demand = Some(demand);
                    if self
                        .terminal_sent
                        .is_some_and(|(epoch, _)| epoch != demand.decode_seek_epoch)
                    {
                        self.terminal_sent = None;
                    }
                }
                _ => {}
            }
        }

        if let Some(spec) = self.active_spec {
            self.prepare_buffers(spec, self.required_frames.get())?;
        }
        Ok(())
    }

    pub(crate) fn can_step(&mut self) -> bool {
        if self.role == SourceAudioRole::Authoritative
            && self.active_spec.is_some()
            && self.demand.is_none()
        {
            return false;
        }
        if !self.command_inlet.has_producer()
            || !self.outputs.data.has_consumer()
            || self.active_spec.is_none()
            || self.demand.is_none()
        {
            return true;
        }
        self.capture_ready()
    }

    pub(crate) fn capture(
        &mut self,
        chunk: &PcmChunk,
        decode_seek_epoch: u64,
    ) -> Result<SourceAudioCaptureOutcome, SourceAudioError> {
        let Some(spec) = self.active_spec else {
            return Ok(SourceAudioCaptureOutcome::Ignored);
        };
        let Some(demand) = self.demand else {
            return Ok(SourceAudioCaptureOutcome::Ignored);
        };
        if !self.outputs.data.has_consumer() {
            return Ok(SourceAudioCaptureOutcome::Ignored);
        }
        if decode_seek_epoch != demand.decode_seek_epoch {
            return Ok(SourceAudioCaptureOutcome::Ignored);
        }
        if chunk.spec() != spec {
            return Err(SourceAudioError::SpecMismatch);
        }

        let chunk_range =
            SourceFrameRange::with_len(chunk.meta.frame_offset, u64::from(chunk.meta.frames))?;
        if self.role == SourceAudioRole::Authoritative
            && chunk_range.start() >= demand.coverage.end()
        {
            self.demand = None;
            return Ok(SourceAudioCaptureOutcome::DemandComplete);
        }
        if !chunk_range.intersects(demand.coverage) {
            return Ok(SourceAudioCaptureOutcome::Ignored);
        }
        let capture_frames = usize::try_from(chunk_range.len())
            .map_err(|_| SourceAudioError::SampleCountOverflow)?;
        if capture_frames > self.prepared_frames {
            self.required_frames =
                NonZeroUsize::new(capture_frames).ok_or(SourceAudioError::SampleCountOverflow)?;
            return Ok(SourceAudioCaptureOutcome::PreparationPending);
        }
        let chunk_samples = sample_count(chunk_range, spec)?;
        if chunk.samples.len() != chunk_samples {
            return Err(SourceAudioError::SampleCountMismatch {
                expected: chunk_samples,
                actual: chunk.samples.len(),
            });
        }
        if !self.capture_ready() {
            return Err(SourceAudioError::DataBackpressure);
        }

        let capture_samples = chunk_samples;
        let mut samples = self
            .buffers
            .pop()
            .ok_or(SourceAudioError::BufferBankNotPrepared)?;
        if samples.len() < capture_samples {
            self.buffers.push(samples);
            return Err(SourceAudioError::BufferBankNotPrepared);
        }
        samples[..capture_samples].copy_from_slice(&chunk.samples);
        let window = SourceAudioWindow::validated(chunk_range, spec, samples, capture_samples);
        let packet = SourceAudioPacket { demand, window };
        if let Err(packet) = self.outputs.data.try_push(packet) {
            self.buffers.push(packet.window.release_samples());
            return Err(SourceAudioError::DataBackpressure);
        }
        Ok(SourceAudioCaptureOutcome::Captured)
    }

    fn reclaim_pending_data(&mut self) {
        if self.outputs.data.has_consumer() {
            let _ = self.outputs.data.flush();
            return;
        }
        if let Some(packet) = self.outputs.data.take_pending() {
            self.buffers.push(packet.window.release_samples());
        }
        self.active_spec = None;
        self.demand = None;
    }

    fn prepare_buffers(
        &mut self,
        spec: PcmSpec,
        required_frames: usize,
    ) -> Result<(), SourceAudioError> {
        let max_samples = required_frames
            .checked_mul(usize::from(spec.channels))
            .ok_or(SourceAudioError::SampleCountOverflow)?;
        for samples in &mut self.buffers {
            samples
                .ensure_len(max_samples)
                .map_err(|_| SourceAudioError::BufferBudgetExhausted)?;
        }
        self.prepared_spec = Some(spec);
        self.prepared_frames = required_frames;
        Ok(())
    }

    fn capture_ready(&mut self) -> bool {
        self.active_spec == self.prepared_spec
            && self.required_frames.get() <= self.prepared_frames
            && !self.buffers.is_empty()
            && self.outputs.data.flush()
    }

    pub(crate) fn is_authoritative(&self) -> bool {
        self.active_spec.is_some() && self.role == SourceAudioRole::Authoritative
    }

    pub(crate) fn finish(&mut self, decode_seek_epoch: u64, terminal: SourceAudioTerminal) -> bool {
        if !self.is_authoritative() || !self.outputs.status.has_consumer() {
            return true;
        }
        if !self.outputs.status.flush() {
            return false;
        }
        if self.terminal_sent == Some((decode_seek_epoch, terminal)) {
            return true;
        }
        let status = SourceAudioStatus {
            decode_seek_epoch,
            lane_id: self.lane_id,
            terminal,
        };
        if self.outputs.status.try_push(status).is_err() {
            return false;
        }
        self.terminal_sent = Some((decode_seek_epoch, terminal));
        !self.outputs.status.has_pending()
    }

    #[cfg(test)]
    pub(crate) fn available_buffers(&self) -> usize {
        self.buffers.len()
    }
}

pub(crate) struct SourceAudioOutputs {
    data: Outlet<SourceAudioPacket>,
    status: Outlet<SourceAudioStatus>,
    _close_guard: SourceAudioCloseGuard,
}

impl SourceAudioOutputs {
    pub(crate) fn new(
        data: Outlet<SourceAudioPacket>,
        status: Outlet<SourceAudioStatus>,
        activity: SourceAudioActivity,
    ) -> Self {
        Self {
            data,
            status,
            _close_guard: SourceAudioCloseGuard(activity),
        }
    }
}

struct SourceAudioCloseGuard(SourceAudioActivity);

impl Drop for SourceAudioCloseGuard {
    fn drop(&mut self) {
        self.0.signal();
    }
}
