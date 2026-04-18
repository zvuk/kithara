//! Decoder node for the audio worker.

use std::sync::Arc;

use kithara_decode::PcmChunk;
use kithara_platform::tokio::sync::Notify;
use tracing::trace;

use super::{
    AudioWorkerSource,
    handle::TrackRegistration,
    types::{ServiceClass, TrackId},
};
use crate::{
    pipeline::{fetch::Fetch, track_fsm::TrackStep},
    runtime::{Node, Outlet, TickResult},
};

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
        if self.chunks_sent >= self.preload_chunks && !self.outlet.has_pending() {
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

        // If we had pending chunks that just got flushed to the ring,
        // we might now meet the preload condition.
        if self.chunks_sent >= self.preload_chunks && !self.preloaded {
            self.complete_preload();
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
                let marker = Fetch::new(PcmChunk::default(), true, epoch);
                // We just called `flush()` above, so the overflow slot is guaranteed to be empty.
                // However, we handle the `Err` case defensively to prevent silent EOF drops
                // if the internal FSM or port contracts change in the future.
                if let Ok(()) = self.outlet.try_push(marker) {
                    self.complete_preload();
                    self.eof_sent = true;
                    TickResult::Progress
                } else {
                    debug_assert!(false, "EOF marker rejected — overflow invariant violated");
                    TickResult::Waiting
                }
            }

            TrackStep::Failed => {
                let epoch = self.source.timeline().seek_epoch();
                let marker = Fetch::new(PcmChunk::default(), true, epoch);
                // We just called `flush()` above, so the overflow slot is guaranteed to be empty.
                if let Ok(()) = self.outlet.try_push(marker) {
                    self.complete_preload();
                    // If the failure marker only landed in the overflow slot,
                    // we need at least one more tick to flush it before the
                    // node may be retired.
                    if self.outlet.has_pending() {
                        TickResult::Progress
                    } else {
                        TickResult::Done
                    }
                } else {
                    debug_assert!(
                        false,
                        "Failed marker rejected — overflow invariant violated"
                    );
                    TickResult::Waiting
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

#[cfg(test)]
mod tests {
    use kithara_stream::Timeline;
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{runtime::connect, worker::MockAudioWorkerSource};

    #[kithara::test]
    fn decoder_node_eof_under_backpressure() {
        let notify = Arc::new(Notify::new());
        let (mut outlet, _inlet) = connect::<Fetch<PcmChunk>>(1, None);

        // Fill the ring buffer and the overflow slot
        outlet
            .try_push(Fetch::new(PcmChunk::default(), false, 0))
            .unwrap();
        outlet
            .try_push(Fetch::new(PcmChunk::default(), false, 0))
            .unwrap();
        assert!(outlet.has_pending());

        let timeline = Timeline::new();
        let source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Eof),
            MockAudioWorkerSource::timeline.stub(|each| {
                each.call(matching!()).returns(timeline.clone());
            }),
        )));

        let mut node = DecoderNode {
            source,
            outlet,
            service_class: ServiceClass::default(),
            preload_notify: notify,
            preload_chunks: 1,
            chunks_sent: 0,
            preloaded: false,
            seek_epoch: 0,
            eof_sent: false,
        };

        // Tick 1: flush fails, returns Waiting
        assert_eq!(node.tick(), TickResult::Waiting);
        assert!(!node.eof_sent);

        // Drain the inlet so flush can succeed
        let _ = node.outlet.take_pending();

        // Tick 2: flush succeeds (overflow is empty). Now step_track returns Eof.
        // It pushes the EOF marker.
        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.eof_sent);
        assert!(node.outlet.has_pending()); // EOF marker is now in overflow
    }

    #[kithara::test]
    fn decoder_node_preload_notify_waits_for_ring() {
        let notify = Arc::new(Notify::new());
        let (mut outlet, mut inlet) = connect::<Fetch<PcmChunk>>(1, None);

        // Fill the ring buffer so the next push goes to overflow
        outlet
            .try_push(Fetch::new(PcmChunk::default(), false, 0))
            .unwrap();

        let timeline = Timeline::new();
        let source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Produced(Fetch::new(
                    PcmChunk::default(),
                    false,
                    0,
                ))),
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Blocked(
                    crate::pipeline::track_fsm::WaitingReason::Waiting,
                )),
            MockAudioWorkerSource::timeline.stub(|each| {
                each.call(matching!()).returns(timeline.clone());
            }),
        )));

        let mut node = DecoderNode {
            source,
            outlet,
            service_class: ServiceClass::default(),
            preload_notify: notify.clone(),
            preload_chunks: 1, // We want 1 chunk to trigger preload
            chunks_sent: 0,
            preloaded: false,
            seek_epoch: 0,
            eof_sent: false,
        };

        // Tick 1: flush succeeds (overflow was empty). step_track produces chunk.
        // Chunk goes to overflow because ring is full.
        // chunks_sent becomes 1, but has_pending is true, so preloaded stays false.
        assert_eq!(node.tick(), TickResult::Progress);
        assert_eq!(node.chunks_sent, 1);
        assert!(!node.preloaded);

        // Tick 2: flush fails (ring still full, overflow has chunk).
        assert_eq!(node.tick(), TickResult::Waiting);
        assert!(!node.preloaded);

        // Consumer reads from ring
        let _ = inlet.try_pop();

        // Tick 3: flush succeeds (moves chunk from overflow to ring).
        // Now chunks_sent >= 1 and !has_pending, so complete_preload is called!
        assert_eq!(node.tick(), TickResult::Waiting); // step_track returns Blocked
        assert!(node.preloaded);
    }
}
