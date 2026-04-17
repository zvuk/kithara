//! Decoder node for the audio worker.

use std::sync::Arc;

use kithara_decode::PcmChunk;
use kithara_platform::tokio::sync::Notify;
use kithara_rt::{Node, Outlet, TickResult};
use tracing::trace;

use super::{
    AudioWorkerSource,
    handle::TrackRegistration,
    types::{ServiceClass, TrackId},
};
use crate::pipeline::{fetch::Fetch, track_fsm::TrackStep};

/// A node that decodes audio chunks.
///
/// The source's FSM must be ticked every pass to make progress on
/// non-producing transitions (e.g. completing a seek). Backpressure is
/// absorbed by [`Outlet`]'s built-in overflow slot: each tick first tries
/// to drain that slot before producing more, so the decoder itself is
/// stateless with respect to parked chunks.
pub(crate) struct DecoderNode {
    source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
    outlet: Outlet<Fetch<PcmChunk>>,
    service_class: ServiceClass,
    preload_notify: Arc<Notify>,
    preload_chunks: usize,
    chunks_sent: usize,
    preloaded: bool,
    seek_epoch: u64,
    eof_sent: bool,
}

impl DecoderNode {
    pub(crate) fn from_registration(_track_id: TrackId, reg: TrackRegistration) -> Self {
        let seek_epoch = reg.source.timeline().seek_epoch();
        Self {
            source: reg.source,
            outlet: reg.outlet,
            service_class: reg.service_class,
            preload_notify: reg.preload_notify,
            preload_chunks: reg.preload_chunks,
            chunks_sent: 0,
            preloaded: false,
            seek_epoch,
            eof_sent: false,
        }
    }

    fn mark_preload_progress(&mut self) {
        if self.preloaded {
            return;
        }

        self.chunks_sent += 1;
        if self.chunks_sent >= self.preload_chunks {
            self.complete_preload();
        }
    }

    fn complete_preload(&mut self) {
        if !self.preloaded {
            self.preload_notify.notify_one();
            self.preloaded = true;
        }
    }

    fn sync_seek_epoch(&mut self) {
        let current = self.source.timeline().seek_epoch();
        if current == self.seek_epoch {
            return;
        }

        self.seek_epoch = current;
        // Drop any chunk parked from the previous epoch — it is stale now.
        let _ = self.outlet.take_pending();
        self.chunks_sent = 0;
        self.preloaded = false;
        self.eof_sent = false;
    }
}

impl Node for DecoderNode {
    fn tick(&mut self) -> TickResult {
        self.sync_seek_epoch();

        // Drain any item parked in the outlet's overflow slot before producing
        // more. If the ring is still saturated, we cannot push anything new
        // this tick.
        if !self.outlet.flush() {
            return TickResult::Waiting;
        }

        match self.source.step_track() {
            TrackStep::Produced(fetch) => {
                self.eof_sent = false;
                // Outlet was just drained → try_push is infallible here.
                let _ = self.outlet.try_push(fetch);
                self.mark_preload_progress();
                TickResult::Progress
            }

            TrackStep::StateChanged => {
                self.eof_sent = false;
                TickResult::Progress
            }

            TrackStep::Blocked(reason) => {
                trace!(?reason, "track blocked");
                TickResult::Waiting
            }

            TrackStep::Eof if self.eof_sent => TickResult::Waiting,

            TrackStep::Eof => {
                let epoch = self.source.timeline().seek_epoch();
                let _ = self
                    .outlet
                    .try_push(Fetch::new(PcmChunk::default(), true, epoch));
                self.complete_preload();
                self.eof_sent = true;
                TickResult::Progress
            }

            TrackStep::Failed => {
                let epoch = self.source.timeline().seek_epoch();
                let _ = self
                    .outlet
                    .try_push(Fetch::new(PcmChunk::default(), true, epoch));
                self.complete_preload();
                // If the failure marker only landed in the overflow slot,
                // we need at least one more tick to flush it before the
                // node may be retired.
                if self.outlet.has_pending() {
                    TickResult::Progress
                } else {
                    TickResult::Done
                }
            }
        }
    }

    fn service_class(&self) -> ServiceClass {
        self.service_class
    }

    fn on_cancel(&mut self) {
        self.complete_preload();
    }
}
