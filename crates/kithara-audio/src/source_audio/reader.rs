use kithara_decode::PcmSpec;
use kithara_platform::sync::Arc;

use super::{
    SourceAudioDemand, SourceAudioError, SourceAudioReadOutcome, SourceFrameRange,
    cache::{SourceAudioCache, SourceAudioCacheInsert},
    model::{
        SourceAudioCommand, SourceAudioConnection, SourceAudioPacket, SourceAudioRequest,
        SourceAudioRole, SourceAudioStatus, SourceAudioTerminal, SourceAudioWindow, sample_count,
    },
};
use crate::runtime::{Inlet, Outlet};

/// Nonblocking reader for immutable decoded source-audio windows.
#[non_exhaustive]
pub struct SourceAudioReader {
    connection: Arc<SourceAudioConnection>,
    generation: u64,
    active: bool,
    demand: Option<SourceAudioRequest>,
    command_outlet: Outlet<SourceAudioCommand>,
    data_inlet: Inlet<SourceAudioPacket>,
    status_inlet: Inlet<SourceAudioStatus>,
    trash_outlet: Outlet<SourceAudioWindow>,
    cache: SourceAudioCache,
    pending_retirement: Option<SourceAudioWindow>,
    terminal: Option<SourceAudioStatus>,
}

impl SourceAudioReader {
    pub(crate) fn new(
        command_outlet: Outlet<SourceAudioCommand>,
        data_inlet: Inlet<SourceAudioPacket>,
        status_inlet: Inlet<SourceAudioStatus>,
        trash_outlet: Outlet<SourceAudioWindow>,
        cache: SourceAudioCache,
    ) -> Self {
        Self {
            connection: Arc::new(SourceAudioConnection::default()),
            generation: 0,
            active: false,
            demand: None,
            command_outlet,
            data_inlet,
            status_inlet,
            trash_outlet,
            cache,
            pending_retirement: None,
            terminal: None,
        }
    }

    /// Activate capture for one decoded audio format.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty channel layout, an incompatible cached format, or command
    /// backpressure.
    pub fn activate(&mut self, spec: PcmSpec) -> Result<(), SourceAudioError> {
        self.activate_with_role(spec, SourceAudioRole::Mirror)
    }

    /// Activate capture as the only decoded-audio output path.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty channel layout, an incompatible cached format, or command
    /// backpressure.
    pub fn activate_authoritative(&mut self, spec: PcmSpec) -> Result<(), SourceAudioError> {
        self.activate_with_role(spec, SourceAudioRole::Authoritative)
    }

    fn activate_with_role(
        &mut self,
        spec: PcmSpec,
        role: SourceAudioRole,
    ) -> Result<(), SourceAudioError> {
        if spec.channels == 0 {
            return Err(SourceAudioError::EmptyChannelLayout);
        }
        self.cache.validate_activation(spec)?;
        let command = SourceAudioCommand::Activate { role, spec };
        self.command_outlet
            .try_push(command)
            .map_err(|_| SourceAudioError::CommandBackpressure)?;
        self.cache.activate(spec);
        self.active = true;
        self.demand = None;
        self.terminal = None;
        Ok(())
    }

    /// Stop new capture while retaining immutable cached ranges.
    ///
    /// # Errors
    ///
    /// Returns [`SourceAudioError::CommandBackpressure`] when the command cannot be queued.
    pub fn deactivate(&mut self) -> Result<(), SourceAudioError> {
        let command = SourceAudioCommand::Deactivate;
        self.command_outlet
            .try_push(command)
            .map_err(|_| SourceAudioError::CommandBackpressure)?;
        self.active = false;
        self.demand = None;
        Ok(())
    }

    /// Request a source range and optional forward look-ahead coverage.
    ///
    /// # Errors
    ///
    /// Returns an error when the reader is inactive, the range is empty, frame or generation
    /// arithmetic overflows, or the command cannot be queued.
    pub fn request(
        &mut self,
        range: SourceFrameRange,
        look_ahead_frames: u64,
        decode_seek_epoch: u64,
    ) -> Result<SourceAudioDemand, SourceAudioError> {
        if !self.active {
            return Err(SourceAudioError::Inactive);
        }
        if range.is_empty() {
            return Err(SourceAudioError::EmptyRange);
        }
        let coverage_end = range
            .end()
            .checked_add(look_ahead_frames)
            .ok_or(SourceAudioError::FrameOverflow)?;
        let coverage = SourceFrameRange::new(range.start(), coverage_end)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(SourceAudioError::GenerationExhausted)?;
        let request = SourceAudioRequest {
            generation,
            decode_seek_epoch,
            requested: range,
            coverage,
        };
        self.command_outlet
            .try_push(SourceAudioCommand::Demand(request))
            .map_err(|_| SourceAudioError::CommandBackpressure)?;
        self.generation = generation;
        self.demand = Some(request);
        Ok(SourceAudioDemand {
            connection: self.connection.clone(),
            request,
        })
    }

    /// Copy a complete demand from cache without blocking.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid demand, incompatible output, unavailable reader state, or
    /// a failed source.
    pub fn read_into(
        &mut self,
        demand: &SourceAudioDemand,
        output: &mut [f32],
    ) -> Result<SourceAudioReadOutcome, SourceAudioError> {
        self.read_range_into(demand, demand.request.requested, output)
    }

    /// Copy a complete subrange of the active demand coverage without blocking.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid demand or range, incompatible output, unavailable reader
    /// state, or a failed source.
    pub fn read_range_into(
        &mut self,
        demand: &SourceAudioDemand,
        range: SourceFrameRange,
        output: &mut [f32],
    ) -> Result<SourceAudioReadOutcome, SourceAudioError> {
        self.validate_demand(demand)?;
        if range.start() < demand.request.coverage.start()
            || range.end() > demand.request.coverage.end()
        {
            return Err(SourceAudioError::RangeOutsideDemand);
        }
        let spec = self.cache.spec().ok_or(SourceAudioError::Inactive)?;
        let expected = sample_count(range, spec)?;
        if output.len() != expected {
            return Err(SourceAudioError::OutputSizeMismatch {
                expected,
                actual: output.len(),
            });
        }

        self.poll();
        if !self.cache.contains(range) {
            return match self
                .terminal
                .filter(|status| status.decode_seek_epoch == demand.request.decode_seek_epoch)
            {
                Some(SourceAudioStatus {
                    terminal: SourceAudioTerminal::Eof,
                    ..
                }) => Ok(SourceAudioReadOutcome::Eof),
                Some(SourceAudioStatus {
                    terminal: SourceAudioTerminal::Failed,
                    ..
                }) => Err(SourceAudioError::SourceFailed),
                None if !self.data_inlet.has_producer() => Err(SourceAudioError::SourceFailed),
                None => Ok(SourceAudioReadOutcome::Pending),
            };
        }
        self.cache.copy(range, output)?;
        let frames =
            usize::try_from(range.len()).map_err(|_| SourceAudioError::SampleCountOverflow)?;
        Ok(SourceAudioReadOutcome::Ready { frames })
    }

    /// Return the activated decoded audio format, if known.
    #[must_use]
    pub const fn spec(&self) -> Option<PcmSpec> {
        self.cache.spec()
    }

    /// Move newly captured immutable windows into the local cache.
    ///
    /// This is nonblocking, copies no sample data, and also advances pending
    /// activation or deactivation commands.
    ///
    pub fn poll(&mut self) {
        let _ = self.command_outlet.flush();
        self.flush_retirement();
        self.drain_packets();
        self.drain_status();
    }

    fn validate_demand(&self, demand: &SourceAudioDemand) -> Result<(), SourceAudioError> {
        if !Arc::ptr_eq(&demand.connection, &self.connection) {
            return Err(SourceAudioError::ForeignDemand);
        }
        if self.demand != Some(demand.request) {
            return Err(SourceAudioError::StaleDemand);
        }
        Ok(())
    }

    fn drain_packets(&mut self) {
        while self.pending_retirement.is_none() {
            let Some(packet) = self.data_inlet.try_pop() else {
                break;
            };
            let accepted = self.demand == Some(packet.request)
                && packet.request.coverage.intersects(packet.window.range())
                && self.cache.spec() == Some(packet.window.spec());
            if !accepted {
                self.retire(packet.window);
                continue;
            }
            match self.cache.insert(packet.window) {
                SourceAudioCacheInsert::Stored { evicted } => {
                    if let Some(window) = evicted {
                        self.retire(window);
                    }
                }
                SourceAudioCacheInsert::Rejected { window } => self.retire(window),
            }
        }
    }

    fn drain_status(&mut self) {
        while let Some(status) = self.status_inlet.try_pop() {
            self.terminal = Some(status);
        }
    }

    fn flush_retirement(&mut self) {
        let Some(window) = self.pending_retirement.take() else {
            return;
        };
        if let Err(window) = self.trash_outlet.try_push(window) {
            self.pending_retirement = Some(window);
        }
    }

    fn retire(&mut self, window: SourceAudioWindow) {
        debug_assert!(self.pending_retirement.is_none());
        if let Err(window) = self.trash_outlet.try_push(window) {
            self.pending_retirement = Some(window);
        }
    }
}
