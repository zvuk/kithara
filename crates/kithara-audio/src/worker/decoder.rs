use kithara_decode::PcmChunk;
use kithara_events::{AudioEvent, DeferredBus, Event};
use kithara_platform::{
    sync::Arc,
    time::{Duration, Instant},
};
use kithara_stream::{PlayheadRead, SeekObserve};

use super::{
    AudioWorkerSource, EngineLoad, PreloadGate, handle::TrackRegistration, types::ServiceClass,
};
use crate::{
    pipeline::{
        fetch::Fetch,
        track_fsm::{TrackStep, WaitingReason},
    },
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
    pub(crate) last_buffer_health_emit: Option<Instant>,
    pub(crate) last_engine_load_emit: Option<Instant>,
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
    /// Held seek-observe handle — avoids an Arc clone on every hot
    /// `sync_seek_epoch` tick.
    seek_obs: Arc<dyn SeekObserve>,
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
    playhead: Arc<dyn PlayheadRead>,
    emit: Arc<DeferredBus<Event>>,
    /// Live engine cost meter. When present, each produced chunk records the
    /// tick's decode+effects wall time against the audio it yielded.
    engine_load: Option<Arc<EngineLoad>>,
    outlet: Outlet<Fetch<PcmChunk>>,
    preload_chunks: usize,
}

impl DecoderNode {
    const BUFFER_HEALTH_EMIT_MIN: Duration = Duration::from_millis(250);
    const ENGINE_LOAD_EMIT_MIN: Duration = Duration::from_millis(500);

    fn complete_preload(&mut self) {
        if !self.runtime.preloaded {
            self.preload_gate.signal_epoch(self.runtime.seek_epoch);
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

    /// Record one produced chunk's decode+effects cost into the shared engine
    /// meter.
    fn record_load(&self, busy: Duration, fetch: &Fetch<PcmChunk>) {
        if let Some(load) = self.engine_load.as_ref() {
            load.record(
                busy,
                fetch.data.frames(),
                fetch.data.spec().sample_rate.get(),
            );
        }
    }

    fn maybe_emit_buffer_health(&mut self, now: Instant) {
        if self
            .runtime
            .last_buffer_health_emit
            .is_some_and(|last| now.duration_since(last) < Self::BUFFER_HEALTH_EMIT_MIN)
        {
            return;
        }
        self.runtime.last_buffer_health_emit = Some(now);
        let position = self.playhead.position();
        let decoded_frontier = self.playhead.decoded_frontier();
        let decoded_frontier_ms = decoded_frontier.as_millis().try_into().unwrap_or(u64::MAX);
        let buffered_ms = decoded_frontier
            .saturating_sub(position)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        self.emit.enqueue(
            AudioEvent::BufferHealth {
                buffered_ms,
                decoded_frontier_ms,
                seek_epoch: self.runtime.seek_epoch,
            }
            .into(),
        );
    }

    fn maybe_emit_engine_load(&mut self, now: Instant) {
        let Some(load) = self.engine_load.as_ref() else {
            return;
        };
        if self
            .runtime
            .last_engine_load_emit
            .is_some_and(|last| now.duration_since(last) < Self::ENGINE_LOAD_EMIT_MIN)
        {
            return;
        }
        self.runtime.last_engine_load_emit = Some(now);
        let snapshot = load.snapshot();
        self.emit.enqueue(
            AudioEvent::EngineLoad {
                load: snapshot.load,
                ms_per_chunk: snapshot.ms,
                realtime_factor: snapshot.realtime,
            }
            .into(),
        );
    }

    fn maybe_emit_worker_telemetry(&mut self, now: Instant) {
        self.maybe_emit_buffer_health(now);
        self.maybe_emit_engine_load(now);
    }

    /// Reset preload state when a new seek epoch arrives.
    ///
    /// Fast path: `SeekObserve::take_decoder_seek` is a one-shot
    /// `AtomicBool` armed by `begin`. The typical no-seek tick
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
        if !self.seek_obs.take_decoder_seek() {
            return;
        }
        let current = self.seek_obs.epoch();
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
        let seek_obs = reg.source.seek_observe();
        let seek_epoch = seek_obs.epoch();
        Self {
            seek_obs,
            source: reg.source,
            outlet: reg.outlet,
            trash_inlet: reg.trash_inlet,
            playhead: reg.playhead,
            emit: reg.emit,
            service_class: reg.service_class,
            preload_gate: reg.preload_gate,
            preload_chunks: reg.preload_chunks,
            engine_load: reg.engine_load,
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

    fn recycle(&mut self) {
        while self.trash_inlet.try_pop().is_some() {}
        self.source.flush_deferred();
        self.outlet.flush_wake_signals();
    }

    fn service_class(&self) -> ServiceClass {
        self.service_class.load()
    }

    fn tick(&mut self) -> TickResult {
        self.sync_seek_epoch();

        if !self.outlet.flush() {
            return TickResult::Backpressured;
        }

        if self.runtime.chunks_sent >= self.preload_chunks && !self.runtime.preloaded {
            self.complete_preload();
        }

        let start = Instant::now();
        let result = match self.source.step_track() {
            TrackStep::Produced(fetch) => {
                self.record_load(start.elapsed(), &fetch);
                self.runtime.eof_sent = false;
                let _ = self.outlet.try_push(fetch);
                self.mark_preload_progress();
                TickResult::Progress
            }

            TrackStep::StateChanged => {
                self.runtime.eof_sent = false;
                TickResult::Progress
            }

            TrackStep::Blocked(reason) => match reason {
                WaitingReason::WaitingDemand => TickResult::UpstreamPending,
                WaitingReason::Waiting | WaitingReason::WaitingMetadata => TickResult::Waiting,
            },

            TrackStep::Eof if self.runtime.eof_sent => TickResult::Backpressured,

            TrackStep::Eof => {
                let epoch = self.source.decode_epoch();
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
                let epoch = self.source.decode_epoch();
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
        };
        self.maybe_emit_worker_telemetry(Instant::now());
        result
    }

    fn warm_up(&mut self) {
        self.source.warm_up();
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::{AudioEvent, Event, EventBus};
    use kithara_platform::time::Duration;
    use kithara_stream::{PlayheadState, PlayheadWrite, SeekControl, SeekObserve, SeekState};
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
        seek_obs: Arc<dyn SeekObserve>,
    ) -> DecoderNode {
        let (_trash_outlet, trash_inlet) = connect::<PcmChunk>(4, None);
        DecoderNode {
            seek_obs,
            source,
            outlet,
            trash_inlet,
            preload_gate,
            playhead: Arc::new(PlayheadState::new()) as Arc<dyn PlayheadRead>,
            emit: Arc::new(DeferredBus::new(EventBus::new(8), 8)),
            service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
            preload_chunks: 1,
            engine_load: None,
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

        let source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Eof),
            MockAudioWorkerSource::decode_epoch.stub(|each| {
                each.call(matching!()).returns(0u64);
            }),
        )));

        let mut node = test_node(
            source,
            outlet,
            gate,
            Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        );

        assert_eq!(node.tick(), TickResult::Backpressured);
        assert!(!node.runtime.eof_sent);

        let _ = node.outlet.take_pending();

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.runtime.eof_sent);
        assert!(node.outlet.has_pending());
    }

    #[kithara::test]
    fn decoder_node_records_engine_load_on_produced() {
        use std::num::NonZero;

        use kithara_bufpool::PcmPool;
        use kithara_decode::{PcmMeta, PcmSpec};

        let meter = Arc::new(EngineLoad::default());
        assert!(!meter.snapshot().is_active(), "idle before any tick");

        let (outlet, _inlet) = connect::<Fetch<PcmChunk>>(4, None);
        let chunk = PcmChunk::new(
            PcmMeta {
                spec: PcmSpec {
                    channels: 2,
                    sample_rate: NonZero::new(44_100).unwrap(),
                },
                frames: 4_410,
                ..Default::default()
            },
            PcmPool::default().attach(vec![0.0f32; 4_410 * 2]),
        );
        let source = Box::new(Unimock::new(
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Produced(Fetch::data(chunk, 0))),
        ));

        let (_trash_outlet, trash_inlet) = connect::<PcmChunk>(4, None);
        let mut node = DecoderNode {
            source,
            outlet,
            trash_inlet,
            seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
            preload_gate: Arc::new(PreloadGate::default()),
            playhead: Arc::new(PlayheadState::new()) as Arc<dyn PlayheadRead>,
            emit: Arc::new(DeferredBus::new(EventBus::new(8), 8)),
            service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
            preload_chunks: 1,
            engine_load: Some(Arc::clone(&meter)),
            runtime: DecoderRuntime::default(),
        };

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(
            meter.snapshot().is_active(),
            "engine meter records on a Produced tick: {:?}",
            meter.snapshot()
        );
    }

    #[kithara::test]
    fn worker_telemetry_throttles_immediate_repeats() {
        let (outlet, _inlet) = connect::<Fetch<PcmChunk>>(4, None);
        let source = Box::new(Unimock::new(()));
        let gate = Arc::new(PreloadGate::default());
        let seek = Arc::new(SeekState::new());
        let playhead = Arc::new(PlayheadState::new());
        playhead.set_position(Duration::from_millis(100));
        playhead.set_decoded_frontier(Duration::from_millis(350));
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let emit = Arc::new(DeferredBus::new(bus, 8));
        let meter = Arc::new(EngineLoad::default());
        meter.record(Duration::from_millis(5), 4_410, 44_100);

        let mut node = DecoderNode {
            seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
            source,
            outlet,
            trash_inlet: connect::<PcmChunk>(4, None).1,
            preload_gate: gate,
            playhead: Arc::clone(&playhead) as Arc<dyn PlayheadRead>,
            emit: Arc::clone(&emit),
            service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
            preload_chunks: 1,
            engine_load: Some(meter),
            runtime: DecoderRuntime::default(),
        };

        let now = kithara_platform::time::Instant::now();
        node.maybe_emit_worker_telemetry(now);
        node.maybe_emit_worker_telemetry(now);
        emit.flush();

        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(Event::Audio(AudioEvent::BufferHealth {
                buffered_ms: 250,
                decoded_frontier_ms: 350,
                seek_epoch: 0,
            }))
        ));
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(Event::Audio(AudioEvent::EngineLoad { .. }))
        ));
        assert!(
            events.try_recv().is_err(),
            "second immediate tick stays throttled"
        );
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
        let eof_source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Eof),
            MockAudioWorkerSource::decode_epoch.stub(|each| {
                each.call(matching!()).returns(0u64);
            }),
        )));
        let mut eof_node = test_node(
            eof_source,
            eof_outlet,
            Arc::clone(&gate),
            Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        );
        assert_eq!(eof_node.tick(), TickResult::Progress);
        let eof_kind = drain_marker_kind(&mut eof_node.outlet, &mut eof_inlet);

        let (failed_outlet, mut failed_inlet) = connect::<Fetch<PcmChunk>>(1, None);
        let failed_source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Failed),
            MockAudioWorkerSource::decode_epoch.stub(|each| {
                each.call(matching!()).returns(0u64);
            }),
        )));
        let mut failed_node = test_node(
            failed_source,
            failed_outlet,
            gate,
            Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        );
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
    fn eof_marker_carries_decode_epoch_not_live_seek_epoch() {
        // Regression (oversubscription false-EOF): a near-end seek (decode
        // epoch N) drives the decoder to a genuine EOF in the same window where
        // a newer consumer seek has already bumped the *seek* epoch to N+1 (the
        // consumer bumps it the instant it requests a seek, long before the
        // worker applies it). The EOF marker must carry the PRODUCER's decode
        // epoch (N) — `decode_epoch()` — not the live `seek_observe().epoch()`
        // (N+1). Stamping the live epoch makes the stale end-of-stream pass the
        // consumer's epoch validator as the *new* seek's terminal, surfacing a
        // false `ReadOutcome::Eof` for an in-range seek.
        use crate::pipeline::fetch::FetchKind;

        let gate = Arc::new(PreloadGate::default());
        let (outlet, mut inlet) = connect::<Fetch<PcmChunk>>(1, None);

        // Consumer already requested the next seek: the seek epoch is now 1,
        // ahead of the decode epoch (0) the pending EOF belongs to.
        let seek_state = SeekState::new();
        let live_epoch = seek_state.begin(Duration::from_secs(1));
        assert_eq!(live_epoch, 1, "begin bumps the seek epoch to 1");
        let seek_obs = Arc::new(seek_state) as Arc<dyn SeekObserve>;

        let source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Eof),
            MockAudioWorkerSource::decode_epoch
                .next_call(matching!())
                .returns(0u64),
        )));

        let mut node = test_node(source, outlet, gate, seek_obs);
        assert_eq!(node.tick(), TickResult::Progress);

        node.outlet.flush();
        let marker = inlet.try_pop().expect("producer pushed an EOF marker");
        assert_eq!(marker.kind, FetchKind::NaturalEof);
        assert_eq!(
            marker.epoch, 0,
            "EOF marker must carry the producer's decode epoch (0), not the live \
             seek epoch (1) the consumer already advanced"
        );
    }

    #[kithara::test]
    fn decoder_node_preload_gate_waits_for_ring() {
        let gate = Arc::new(PreloadGate::default());
        let (mut outlet, mut inlet) = connect::<Fetch<PcmChunk>>(1, None);

        outlet
            .try_push(Fetch::new(PcmChunk::default(), false, 0))
            .unwrap();

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
                .returns(TrackStep::Blocked(WaitingReason::Waiting)),
        )));

        let mut node = test_node(
            source,
            outlet,
            Arc::clone(&gate),
            Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        );

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
    fn decoder_node_live_upstream_demand_does_not_tick_hang_wait() {
        let gate = Arc::new(PreloadGate::default());
        let (outlet, _inlet) = connect::<Fetch<PcmChunk>>(2, None);

        let source = Box::new(Unimock::new(
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Blocked(WaitingReason::WaitingDemand)),
        ));

        let mut node = test_node(
            source,
            outlet,
            gate,
            Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        );

        assert_eq!(node.tick(), TickResult::UpstreamPending);
    }

    #[kithara::test]
    fn decoder_node_seek_rearms_preload_gate() {
        let gate = Arc::new(PreloadGate::default());
        let (outlet, mut inlet) = connect::<Fetch<PcmChunk>>(2, None);

        let seek_state = Arc::new(SeekState::new());
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
        )));

        // Pass a seek_obs handle derived from `seek_state` so begin()
        // arms the shared latch the node will observe on its next tick.
        let mut node = test_node(
            source,
            outlet,
            Arc::clone(&gate),
            Arc::clone(&seek_state) as Arc<dyn SeekObserve>,
        );

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.runtime.preloaded);
        assert!(gate.is_ready(), "first chunk opens the gate");

        let epoch = SeekControl::begin(&*seek_state, Duration::from_secs(1));

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(!node.runtime.preloaded, "seek resets the preload runtime");
        assert!(!gate.is_ready(), "sync_seek_epoch re-closes the gate");

        let _ = inlet.try_pop();

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.runtime.preloaded);
        assert!(gate.is_ready(), "post-seek refill reopens the gate");
        assert!(
            gate.is_ready_for_epoch(epoch),
            "post-seek refill must open the new seek epoch",
        );
    }
}
