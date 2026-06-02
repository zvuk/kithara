use std::sync::Arc;

use kithara_decode::PcmChunk;
use tracing::trace;

use super::{AudioWorkerSource, PreloadGate, handle::TrackRegistration, types::ServiceClass};
use crate::{
    pipeline::{fetch::Fetch, track_fsm::TrackStep},
    runtime::{AtomicServiceClass, Inlet, Node, Outlet, TickResult},
};

/// Per-tick state of a [`DecoderNode`] — preload progress, EOF flag, and
/// the cached seek epoch — bundled so the constructor and the
/// epoch-reset path can spell `DecoderRuntime::default()` instead of
/// listing each zero field at every call site.
#[derive(Default)]
#[non_exhaustive]
pub(crate) struct DecoderRuntime {
    pub(crate) eof_sent: bool,
    pub(crate) preloaded: bool,
    pub(crate) seek_epoch: u64,
    pub(crate) chunks_sent: usize,
}

/// A node that decodes audio chunks.
///
/// The source's FSM must be ticked every pass to make progress on
/// non-producing transitions (e.g. completing a seek). Backpressure is
/// absorbed by [`Outlet`]'s built-in overflow slot: each tick first tries
/// to drain that slot before producing more, so the decoder itself is
/// stateless with respect to parked chunks.
pub(crate) struct DecoderNode {
    preload_gate: Arc<PreloadGate>,
    /// Shared priority hint written wait-free by the real-time consumer and
    /// read back here by the scheduler each pass — see [`AtomicServiceClass`].
    service_class: Arc<AtomicServiceClass>,
    source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
    runtime: DecoderRuntime,
    /// Spent chunks returned by the real-time consumer. Drained once per pass
    /// by [`recycle`](DecoderNode::recycle), in the scheduler's unchecked shell
    /// before the produce core, so the pooled buffers are freed/recycled on
    /// this worker thread, never on the audio thread.
    trash_inlet: Inlet<PcmChunk>,
    outlet: Outlet<Fetch<PcmChunk>>,
    preload_chunks: usize,
}

impl DecoderNode {
    fn complete_preload(&mut self) {
        if !self.runtime.preloaded {
            self.preload_gate.signal();
            self.runtime.preloaded = true;
        }
    }

    fn mark_preload_progress(&mut self) {
        if self.runtime.preloaded {
            return;
        }

        self.runtime.chunks_sent += 1;
        if self.runtime.chunks_sent >= self.preload_chunks && !self.outlet.has_pending() {
            self.complete_preload();
        }
    }

    /// Reset preload state when a new seek epoch arrives.
    ///
    /// Fast path: `Timeline::take_decoder_node_seek` is a one-shot
    /// `AtomicBool` armed by `initiate_seek`. The typical no-seek tick
    /// reads a single bool and falls through; only the rare epoch-bump
    /// tick goes through the `Arc<AtomicU64>` deref to refresh the
    /// cached value. The slow path still re-reads the canonical
    /// `seek_epoch` so a spurious latch consume costs at most one
    /// no-op compare.
    ///
    /// On an actual epoch bump the preload gate is re-armed (re-closed)
    /// so a fresh `Resource::preload().await` blocks again until the
    /// post-seek refill re-opens it.
    fn sync_seek_epoch(&mut self) {
        if !self.source.timeline().did_take_decoder_node_seek() {
            return;
        }
        let current = self.source.timeline().seek_epoch();
        if current == self.runtime.seek_epoch {
            return;
        }

        let _ = self.outlet.take_pending();
        self.preload_gate.rearm();
        self.runtime = DecoderRuntime {
            seek_epoch: current,
            ..Default::default()
        };
    }
}

impl From<TrackRegistration> for DecoderNode {
    fn from(reg: TrackRegistration) -> Self {
        let seek_epoch = reg.source.timeline().seek_epoch();
        Self {
            source: reg.source,
            outlet: reg.outlet,
            trash_inlet: reg.trash_inlet,
            service_class: reg.service_class,
            preload_gate: reg.preload_gate,
            preload_chunks: reg.preload_chunks,
            runtime: DecoderRuntime {
                seek_epoch,
                ..Default::default()
            },
        }
    }
}

impl Node for DecoderNode {
    fn on_cancel(&mut self) {
        self.complete_preload();
    }

    fn service_class(&self) -> ServiceClass {
        self.service_class.load()
    }

    fn recycle(&mut self) {
        while self.trash_inlet.try_pop().is_some() {}
        self.source.flush_deferred();
    }

    fn warm_up(&mut self) {
        self.source.warm_up();
    }

    fn tick(&mut self) -> TickResult {
        self.sync_seek_epoch();

        if !self.outlet.flush() {
            return TickResult::Backpressured;
        }

        if self.runtime.chunks_sent >= self.preload_chunks && !self.runtime.preloaded {
            self.complete_preload();
        }

        match self.source.step_track() {
            TrackStep::Produced(fetch) => {
                self.runtime.eof_sent = false;
                let _ = self.outlet.try_push(fetch);
                self.mark_preload_progress();
                TickResult::Progress
            }

            TrackStep::StateChanged => {
                self.runtime.eof_sent = false;
                TickResult::Progress
            }

            TrackStep::Blocked(reason) => {
                trace!(?reason, "track blocked");
                TickResult::Waiting
            }

            TrackStep::Eof if self.runtime.eof_sent => TickResult::Backpressured,

            TrackStep::Eof => {
                let epoch = self.source.timeline().seek_epoch();
                let marker = Fetch::new(PcmChunk::default(), true, epoch);
                if let Ok(()) = self.outlet.try_push(marker) {
                    self.complete_preload();
                    self.runtime.eof_sent = true;
                    TickResult::Progress
                } else {
                    debug_assert!(false, "EOF marker rejected — overflow invariant violated");
                    TickResult::Waiting
                }
            }

            TrackStep::Failed => {
                let epoch = self.source.timeline().seek_epoch();
                let marker = Fetch::failure(PcmChunk::default(), epoch);
                if let Ok(()) = self.outlet.try_push(marker) {
                    self.complete_preload();
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
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_stream::Timeline;
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{
        runtime::{Inlet, Outlet, connect},
        worker::MockAudioWorkerSource,
    };

    /// Build a `DecoderNode` for tests: same defaults across the whole
    /// suite (preload after one chunk, default service class, fresh
    /// runtime), so call sites only spell out what they vary.
    fn test_node(
        source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
        outlet: Outlet<Fetch<PcmChunk>>,
        preload_gate: Arc<PreloadGate>,
    ) -> DecoderNode {
        let (_trash_outlet, trash_inlet) = connect::<PcmChunk>(4, None);
        DecoderNode {
            source,
            outlet,
            trash_inlet,
            preload_gate,
            service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
            preload_chunks: 1,
            runtime: DecoderRuntime::default(),
        }
    }

    #[kithara::test]
    fn decoder_node_eof_under_backpressure() {
        let gate = Arc::new(PreloadGate::default());
        let (mut outlet, _inlet) = connect::<Fetch<PcmChunk>>(1, None);

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

        let mut node = test_node(source, outlet, gate);

        assert_eq!(node.tick(), TickResult::Backpressured);
        assert!(!node.runtime.eof_sent);

        let _ = node.outlet.take_pending();

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.runtime.eof_sent);
        assert!(node.outlet.has_pending());
    }

    #[kithara::test]
    fn decoder_node_distinguishes_failed_from_eof_on_the_wire() {
        use std::fmt::Debug;

        use crate::pipeline::fetch::FetchKind;

        /// Drains one marker off the outlet and returns its `FetchKind`.
        /// The two producer terminal steps (`TrackStep::Eof` /
        /// `TrackStep::Failed`) must materialise as distinct kinds on
        /// the wire so the consumer can finalise the track only on
        /// natural EOF.
        fn drain_marker_kind<T: Debug>(
            outlet: &mut Outlet<Fetch<T>>,
            inlet: &mut Inlet<Fetch<T>>,
        ) -> FetchKind {
            outlet.flush();
            inlet
                .try_pop()
                .expect("producer pushed a terminal marker")
                .kind
        }

        let gate = Arc::new(PreloadGate::default());

        let (eof_outlet, mut eof_inlet) = connect::<Fetch<PcmChunk>>(1, None);
        let eof_timeline = Timeline::new();
        let eof_source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Eof),
            MockAudioWorkerSource::timeline.stub(|each| {
                each.call(matching!()).returns(eof_timeline.clone());
            }),
        )));
        let mut eof_node = test_node(eof_source, eof_outlet, Arc::clone(&gate));
        assert_eq!(eof_node.tick(), TickResult::Progress);
        let eof_kind = drain_marker_kind(&mut eof_node.outlet, &mut eof_inlet);

        let (failed_outlet, mut failed_inlet) = connect::<Fetch<PcmChunk>>(1, None);
        let failed_timeline = Timeline::new();
        let failed_source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Failed),
            MockAudioWorkerSource::timeline.stub(|each| {
                each.call(matching!()).returns(failed_timeline.clone());
            }),
        )));
        let mut failed_node = test_node(failed_source, failed_outlet, gate);
        let _ = failed_node.tick();
        let failed_kind = drain_marker_kind(&mut failed_node.outlet, &mut failed_inlet);

        assert_ne!(
            eof_kind, failed_kind,
            "TrackStep::Eof and TrackStep::Failed must not collapse into \
             the same wire marker — the consumer has to distinguish \
             natural end-of-clip from a transient decoder/source failure, \
             otherwise a post-seek failure cascades into PlayerTrack::\
             Finished and empties the track arena mid-clip"
        );
    }

    #[kithara::test]
    fn decoder_node_preload_gate_waits_for_ring() {
        let gate = Arc::new(PreloadGate::default());
        let (mut outlet, mut inlet) = connect::<Fetch<PcmChunk>>(1, None);

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

        let mut node = test_node(source, outlet, Arc::clone(&gate));

        assert_eq!(node.tick(), TickResult::Progress);
        assert_eq!(node.runtime.chunks_sent, 1);
        assert!(!node.runtime.preloaded);
        assert!(!gate.is_ready());

        assert_eq!(node.tick(), TickResult::Backpressured);
        assert!(!node.runtime.preloaded);
        assert!(!gate.is_ready());

        let _ = inlet.try_pop();

        assert_eq!(node.tick(), TickResult::Waiting);
        assert!(node.runtime.preloaded);
        assert!(gate.is_ready());
    }

    #[kithara::test]
    fn decoder_node_seek_rearms_preload_gate() {
        let gate = Arc::new(PreloadGate::default());
        let (outlet, mut inlet) = connect::<Fetch<PcmChunk>>(2, None);

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
                .returns(TrackStep::StateChanged),
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Produced(Fetch::new(
                    PcmChunk::default(),
                    false,
                    0,
                ))),
            MockAudioWorkerSource::timeline.stub(|each| {
                each.call(matching!()).returns(timeline.clone());
            }),
        )));

        let mut node = test_node(source, outlet, Arc::clone(&gate));

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.runtime.preloaded);
        assert!(gate.is_ready(), "first chunk opens the gate");

        let _ = timeline.seek_control().begin(Duration::from_secs(1));

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(!node.runtime.preloaded, "seek resets the preload runtime");
        assert!(!gate.is_ready(), "sync_seek_epoch re-closes the gate");

        let _ = inlet.try_pop();

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.runtime.preloaded);
        assert!(gate.is_ready(), "post-seek refill reopens the gate");
    }
}
