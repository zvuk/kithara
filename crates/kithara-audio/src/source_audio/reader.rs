use std::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use kithara_decode::PcmSpec;

use super::{
    SourceAudioActivity, SourceAudioDemand, SourceAudioError, SourceAudioReadOutcome,
    SourceFrameRange,
    cache::{SourceAudioCache, SourceAudioCacheInsert},
    model::{
        SourceAudioCommand, SourceAudioPacket, SourceAudioRole, SourceAudioStatus,
        SourceAudioTerminal, SourceAudioWindow, sample_count,
    },
};
use crate::runtime::{Inlet, Outlet};

static NEXT_LANE_ID: AtomicU64 = AtomicU64::new(1);

/// Nonblocking reader for immutable decoded source-audio windows.
#[non_exhaustive]
pub struct SourceAudioReader {
    lane_id: NonZeroU64,
    generation: u64,
    active: bool,
    spec: Option<PcmSpec>,
    demand: Option<SourceAudioDemand>,
    command_outlet: Outlet<SourceAudioCommand>,
    data_inlet: Inlet<SourceAudioPacket>,
    status_inlet: Inlet<SourceAudioStatus>,
    trash_outlet: Outlet<SourceAudioWindow>,
    cache: SourceAudioCache,
    pending_retirement: Option<SourceAudioWindow>,
    terminal: Option<SourceAudioStatus>,
    activity: Option<SourceAudioActivity>,
}

impl SourceAudioReader {
    pub(crate) fn new(
        lane_id: NonZeroU64,
        command_outlet: Outlet<SourceAudioCommand>,
        data_inlet: Inlet<SourceAudioPacket>,
        status_inlet: Inlet<SourceAudioStatus>,
        trash_outlet: Outlet<SourceAudioWindow>,
        cache: SourceAudioCache,
        activity: SourceAudioActivity,
    ) -> Self {
        Self {
            lane_id,
            generation: 0,
            active: false,
            spec: None,
            demand: None,
            command_outlet,
            data_inlet,
            status_inlet,
            trash_outlet,
            cache,
            pending_retirement: None,
            terminal: None,
            activity: Some(activity),
        }
    }

    /// Transfer the activity edge to the external waiter.
    /// Returns `None` after the handle has already been taken.
    #[must_use = "the source activity handle can be taken only once"]
    pub fn take_activity(&mut self) -> Option<SourceAudioActivity> {
        self.activity.take()
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
        if self.spec.is_some_and(|current| current != spec) && !self.cache.is_empty() {
            return Err(SourceAudioError::SpecMismatch);
        }
        let command = SourceAudioCommand::Activate {
            lane_id: self.lane_id,
            role,
            spec,
        };
        self.command_outlet
            .try_push(command)
            .map_err(|_| SourceAudioError::CommandBackpressure)?;
        self.active = true;
        self.spec = Some(spec);
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
        let command = SourceAudioCommand::Deactivate {
            lane_id: self.lane_id,
        };
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
        let demand = SourceAudioDemand {
            lane_id: self.lane_id,
            generation,
            decode_seek_epoch,
            requested: range,
            coverage,
        };
        self.command_outlet
            .try_push(SourceAudioCommand::Demand(demand))
            .map_err(|_| SourceAudioError::CommandBackpressure)?;
        self.generation = generation;
        self.demand = Some(demand);
        Ok(demand)
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
        self.read_range_into(demand, demand.requested, output)
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
        self.validate_demand(*demand)?;
        if range.start() < demand.coverage.start() || range.end() > demand.coverage.end() {
            return Err(SourceAudioError::RangeOutsideDemand);
        }
        let spec = self.spec.ok_or(SourceAudioError::Inactive)?;
        let expected = sample_count(range, spec)?;
        if output.len() != expected {
            return Err(SourceAudioError::OutputSizeMismatch {
                expected,
                actual: output.len(),
            });
        }

        self.poll();
        if !self.cache.contains(range) {
            return match self.terminal.filter(|status| {
                status.lane_id == self.lane_id
                    && status.decode_seek_epoch == demand.decode_seek_epoch
            }) {
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
        self.spec
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

    fn validate_demand(&self, demand: SourceAudioDemand) -> Result<(), SourceAudioError> {
        if demand.lane_id != self.lane_id {
            return Err(SourceAudioError::ForeignDemand);
        }
        if self.demand != Some(demand) {
            return Err(SourceAudioError::StaleDemand);
        }
        Ok(())
    }

    fn drain_packets(&mut self) {
        while self.pending_retirement.is_none() {
            let Some(packet) = self.data_inlet.try_pop() else {
                break;
            };
            let accepted = self.demand == Some(packet.demand)
                && packet.demand.coverage.intersects(packet.window.range())
                && self.spec == Some(packet.window.spec());
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
            if status.lane_id == self.lane_id {
                self.terminal = Some(status);
            }
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

pub(crate) fn next_lane_id() -> Result<NonZeroU64, SourceAudioError> {
    let lane_id = NEXT_LANE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| SourceAudioError::LaneExhausted)?;
    NonZeroU64::new(lane_id).ok_or(SourceAudioError::LaneExhausted)
}
