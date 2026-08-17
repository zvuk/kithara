use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, PcmChunk};
use kithara_events::{AudioEvent, DeferredBus, Event};
use kithara_platform::{
    sync::Arc,
    time::{Duration, Instant},
};
use kithara_stream::{PlayheadRead, SeekObserve};

use super::{
    AudioWorkerSource, EngineLoad, OutputDisposition, PreloadGate, PresentResult, Presentation,
    PresentationPublisher, PresentedBlock, PresentedPcm, ServiceClass,
};
use crate::{
    pipeline::{
        config::PresentationChain,
        fetch::Fetch,
        track::{TrackStep, WaitingReason},
    },
    runtime::{AtomicServiceClass, Inlet, Node, StrictOutlet, TickResult},
};

/// Everything needed to register a track with the shared worker.
pub(crate) struct TrackRegistration {
    pub(crate) emit: Arc<DeferredBus<Event>>,
    pub(crate) playhead: Arc<dyn PlayheadRead>,
    pub(crate) preload_gate: Arc<PreloadGate>,
    /// Shared priority hint. The real-time consumer writes it wait-free
    /// (`Audio::set_service_class`); the worker scheduler reads it each pass.
    pub(crate) service_class: Arc<AtomicServiceClass>,
    pub(crate) source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
    /// Final-output return ring: the real-time consumer ([`crate::Audio`])
    /// reports whether each output was returned or detached, so replacement
    /// allocation and pooled-buffer recycling stay on the worker thread.
    pub(crate) trash_inlet: Inlet<OutputDisposition>,
    pub(crate) engine_load: Option<Arc<EngineLoad>>,
    pub(crate) chain: PresentationChain,
    pub(crate) initial_spec: kithara_decode::PcmSpec,
    pub(crate) outlet: StrictOutlet<Fetch<PresentedPcm>>,
    pub(crate) pcm_pool: PcmPool,
    pub(crate) preload_chunks: usize,
    pub(crate) presentation: PresentationPublisher,
    pub(crate) raw_buffer_chunks: usize,
}

/// Per-tick state of a [`DecoderNode`] — preload progress, EOF flag, and
/// the cached seek epoch — bundled so the constructor and the
/// epoch-reset path can spell `DecoderRuntime::default()` instead of
/// listing each zero field at every call site.
#[derive(Default)]
#[non_exhaustive]
pub(crate) struct DecoderRuntime {
    pub(crate) last_buffer_health_emit: Option<Instant>,
    pub(crate) last_engine_load_emit: Option<Instant>,
    pub(crate) eof_sent: bool,
    pub(crate) preloaded: bool,
    pub(crate) seek_epoch: u64,
    pub(crate) chunks_sent: usize,
}

/// A node that fills a deep raw queue and presents through one effect chain.
pub(crate) struct DecoderNode {
    emit: Arc<DeferredBus<Event>>,
    playhead: Arc<dyn PlayheadRead>,
    preload_gate: Arc<PreloadGate>,
    /// Held seek-observe handle — avoids an Arc clone on every hot
    /// `sync_seek_epoch` tick.
    seek_obs: Arc<dyn SeekObserve>,
    /// Shared priority hint written wait-free by the real-time consumer and
    /// read back here by the scheduler each pass — see [`AtomicServiceClass`].
    service_class: Arc<AtomicServiceClass>,
    source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
    runtime: DecoderRuntime,
    /// Final-output dispositions from the real-time consumer. Drained once per
    /// pass by [`recycle`](DecoderNode::recycle), in the scheduler's unchecked
    /// shell before the produce core.
    trash_inlet: Inlet<OutputDisposition>,
    /// Live engine cost meter. When present, each produced chunk records the
    /// tick's decode+effects wall time against the audio it yielded.
    engine_load: Option<Arc<EngineLoad>>,
    pending_failure: Option<DecodeError>,
    presentation: Presentation,
    preload_chunks: usize,
}

impl DecoderNode {
    const BUFFER_HEALTH_EMIT_MIN: Duration = Duration::from_millis(250);
    const ENGINE_LOAD_EMIT_MIN: Duration = Duration::from_millis(500);

    fn defer_failure(&mut self, error: DecodeError) {
        if self.pending_failure.is_none() {
            self.pending_failure = Some(error);
        }
    }

    fn complete_preload(&mut self) {
        if !self.runtime.preloaded {
            self.preload_gate.signal_epoch(self.runtime.seek_epoch);
            self.runtime.preloaded = true;
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
                load: snapshot.load(),
                ms_per_chunk: snapshot.ms(),
                realtime_factor: snapshot.realtime(),
            }
            .into(),
        );
    }

    fn maybe_emit_worker_telemetry(&mut self, now: Instant) {
        self.maybe_emit_buffer_health(now);
        self.maybe_emit_engine_load(now);
    }

    fn record_load(&self, busy: Duration, block: PresentedBlock) {
        if let Some(load) = self.engine_load.as_ref() {
            load.record(busy, block.frames, block.sample_rate);
        }
    }

    fn service_seek_epoch(&mut self) {
        if !self.seek_obs.take_decoder_seek() {
            return;
        }
        let current = self.seek_obs.epoch();
        if current == self.runtime.seek_epoch {
            return;
        }

        let source = &self.source;
        if let Err(error) = self
            .presentation
            .reset_epoch(current, |chunk| source.retire_chunk(chunk))
        {
            self.source.presentation_failed(error);
            self.presentation.finish_failed(current);
        }
        self.preload_gate.rearm();
        self.runtime = DecoderRuntime {
            seek_epoch: current,
            ..Default::default()
        };
    }

    fn present_once(&mut self, started: Instant) -> PresentResult {
        let result = {
            let source = &self.source;
            self.presentation.step(|chunk| source.retire_chunk(chunk))
        };
        match result {
            Ok(PresentResult::Produced(block)) => {
                self.record_load(started.elapsed(), block);
                PresentResult::Produced(block)
            }
            Ok(result) => result,
            Err(error) => {
                let epoch = self.source.decode_epoch();
                self.defer_failure(error);
                let source = &self.source;
                if let Err(error) = self
                    .presentation
                    .abort_failed(epoch, |chunk| source.retire_chunk(chunk))
                {
                    self.defer_failure(error);
                    self.presentation.finish_failed(epoch);
                }
                PresentResult::Advanced
            }
        }
    }

    fn fail_presentation(&mut self, detail: &'static str) {
        let epoch = self.source.decode_epoch();
        self.defer_failure(DecodeError::InvalidData { detail });
        self.presentation.finish_failed(epoch);
    }

    fn should_present(&self) -> bool {
        self.runtime.preloaded
            || self.presentation.raw_ready_for_preload(self.preload_chunks)
            || self.presentation.is_raw_full()
    }

    fn pump_source(&mut self) -> TickResult {
        if let Some((epoch, spec)) = self.source.take_presentation_barrier() {
            let barrier = super::PresentationBarrier::DecoderReplaced { epoch, spec };
            if barrier.epoch() != self.presentation.epoch() {
                return TickResult::Progress;
            }
            if self.presentation.admit_barrier(barrier).is_err() {
                self.fail_presentation("presentation barrier rejected after capacity preflight");
            }
            return TickResult::Progress;
        }
        match self.source.step_track() {
            TrackStep::Produced(fetch) => {
                self.runtime.eof_sent = false;
                if fetch.epoch() != self.presentation.epoch() {
                    if let Fetch::Data { data, .. } = fetch {
                        self.source.retire_chunk(data);
                    }
                    return TickResult::Progress;
                }
                if let Some(rejected) = self.presentation.admit(fetch) {
                    if let Fetch::Data { data, .. } = rejected {
                        self.source.retire_chunk(data);
                    }
                    self.fail_presentation("raw PCM rejected after capacity preflight");
                } else {
                    self.runtime.chunks_sent = self.runtime.chunks_sent.saturating_add(1);
                }
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
            TrackStep::Eof => {
                let epoch = self.source.decode_epoch();
                if epoch == self.presentation.epoch() {
                    self.presentation.finish_eof(epoch);
                }
                TickResult::Progress
            }
            TrackStep::Failed => {
                let epoch = self.source.decode_epoch();
                if epoch == self.presentation.epoch() {
                    self.presentation.finish_failed(epoch);
                }
                TickResult::Progress
            }
        }
    }
}

impl From<TrackRegistration> for DecoderNode {
    fn from(reg: TrackRegistration) -> Self {
        let seek_obs = reg.source.seek_observe();
        let seek_epoch = seek_obs.epoch();
        let presentation = Presentation::new(
            reg.raw_buffer_chunks,
            reg.chain,
            reg.pcm_pool,
            reg.initial_spec,
            reg.outlet,
            reg.presentation,
            seek_epoch,
        );
        Self {
            seek_obs,
            source: reg.source,
            presentation,
            trash_inlet: reg.trash_inlet,
            playhead: reg.playhead,
            emit: reg.emit,
            service_class: reg.service_class,
            preload_gate: reg.preload_gate,
            preload_chunks: reg.preload_chunks,
            engine_load: reg.engine_load,
            pending_failure: None,
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
        self.presentation.release_rejected_off_rt();
        while let Some(disposition) = self.trash_inlet.try_pop() {
            self.presentation.restore_output(disposition);
            self.presentation.release_rejected_off_rt();
        }
        self.presentation.release_retired_off_rt();
        if let Some(error) = self.pending_failure.take() {
            self.source.presentation_failed(error);
        }
        self.service_seek_epoch();
        self.presentation.service_off_rt();
        self.source.flush_deferred();
        self.presentation.flush_wake_signals();
    }

    fn service_class(&self) -> ServiceClass {
        self.service_class.load()
    }

    fn tick(&mut self) -> TickResult {
        if self.seek_obs.epoch() != self.runtime.seek_epoch {
            return TickResult::Progress;
        }
        let start = Instant::now();
        let mut made_progress = false;
        let mut source_wait = None;
        let mut present_result = PresentResult::Idle;
        let mut presented = false;

        if self.should_present() {
            present_result = self.present_once(start);
            presented = true;
            made_progress |= matches!(
                present_result,
                PresentResult::Advanced | PresentResult::Produced(_) | PresentResult::Terminal
            );
        }

        if !self.presentation.is_raw_full() && !self.presentation.is_terminal() {
            let source_result = self.pump_source();
            match source_result {
                TickResult::Progress => made_progress = true,
                TickResult::Waiting | TickResult::UpstreamPending => {
                    source_wait = Some(source_result);
                }
                TickResult::Backpressured | TickResult::Done => {}
            }
        }

        if !presented && self.should_present() {
            present_result = self.present_once(start);
            made_progress |= matches!(
                present_result,
                PresentResult::Advanced | PresentResult::Produced(_) | PresentResult::Terminal
            );
        }

        if matches!(present_result, PresentResult::Terminal) {
            self.runtime.eof_sent = true;
        }
        if self.presentation.preload_ready(self.preload_chunks) {
            self.complete_preload();
        }

        let result = if self.presentation.terminal_failed() && self.presentation.terminal_sent() {
            TickResult::Done
        } else if made_progress {
            TickResult::Progress
        } else if let Some(wait) = source_wait {
            wait
        } else if matches!(present_result, PresentResult::Backpressured)
            || self.presentation.is_raw_full()
            || self.presentation.terminal_sent()
        {
            TickResult::Backpressured
        } else {
            TickResult::Waiting
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
    use std::{
        num::NonZeroU32,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use assert_no_alloc::assert_no_alloc;
    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmMeta, PcmSpec};
    use kithara_events::{AudioEvent, Event, EventBus};
    use kithara_platform::time::Duration;
    use kithara_stream::{PlayheadState, PlayheadWrite, SeekControl, SeekObserve, SeekState};
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{
        renderer::{MockAudioWorkerSource, presentation::PRESENTATION_FRAMES, presentation_cell},
        runtime::{StrictOutlet, connect, connect_strict},
        traits::PresentationPoint,
    };

    fn test_spec(channels: u16, sample_rate: u32) -> PcmSpec {
        PcmSpec::new(
            channels,
            NonZeroU32::new(sample_rate).expect("test sample rate is non-zero"),
        )
    }

    fn default_test_spec() -> PcmSpec {
        test_spec(1, 48_000)
    }

    fn chunk_with_spec(spec: PcmSpec, frames: usize) -> PcmChunk {
        let mut meta = PcmMeta::default();
        meta.spec = spec;
        meta.frames = u32::try_from(frames).expect("test frame count fits u32");
        PcmChunk::new(
            meta,
            PcmPool::default().attach(vec![0.0; frames * usize::from(spec.channels)]),
        )
    }

    fn single_frame_chunk() -> PcmChunk {
        chunk_with_spec(default_test_spec(), 1)
    }

    fn presented_chunk() -> PresentedPcm {
        let chunk = single_frame_chunk();
        let point = PresentationPoint::new(0, 1, 0, 1, chunk.spec().sample_rate);
        PresentedPcm::new(chunk, point)
    }

    fn presentation_block_chunk() -> PcmChunk {
        chunk_with_spec(default_test_spec(), PRESENTATION_FRAMES)
    }

    struct ConcurrentSeekEofSource {
        seek_state: Arc<SeekState>,
    }

    struct FailureProbeSource {
        failures: Arc<AtomicUsize>,
        seek_state: Arc<SeekState>,
    }

    impl AudioWorkerSource for FailureProbeSource {
        type Chunk = PcmChunk;

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek_state) as Arc<dyn SeekObserve>
        }

        fn step_track(&mut self) -> TrackStep<Self::Chunk> {
            TrackStep::Blocked(WaitingReason::Waiting)
        }

        fn presentation_failed(&mut self, _error: DecodeError) {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl AudioWorkerSource for ConcurrentSeekEofSource {
        type Chunk = PcmChunk;

        fn decode_epoch(&self) -> u64 {
            0
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek_state) as Arc<dyn SeekObserve>
        }

        fn step_track(&mut self) -> TrackStep<Self::Chunk> {
            self.seek_state.begin(Duration::from_secs(1));
            TrackStep::Eof
        }
    }

    /// Build a `DecoderNode` for tests: same defaults across the whole
    /// suite (preload after one chunk, default service class, fresh
    /// runtime), so call sites only spell out what they vary.
    fn test_node(
        source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
        outlet: StrictOutlet<Fetch<PresentedPcm>>,
        preload_gate: Arc<PreloadGate>,
        seek_obs: Arc<dyn SeekObserve>,
    ) -> DecoderNode {
        test_node_with_spec(source, outlet, preload_gate, seek_obs, default_test_spec())
    }

    fn test_node_with_spec(
        source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
        outlet: StrictOutlet<Fetch<PresentedPcm>>,
        preload_gate: Arc<PreloadGate>,
        seek_obs: Arc<dyn SeekObserve>,
        initial_spec: PcmSpec,
    ) -> DecoderNode {
        test_node_with_limits(source, outlet, preload_gate, seek_obs, initial_spec, 4, 1)
    }

    fn test_node_with_limits(
        source: Box<dyn AudioWorkerSource<Chunk = PcmChunk>>,
        outlet: StrictOutlet<Fetch<PresentedPcm>>,
        preload_gate: Arc<PreloadGate>,
        seek_obs: Arc<dyn SeekObserve>,
        initial_spec: PcmSpec,
        raw_buffer_chunks: usize,
        preload_chunks: usize,
    ) -> DecoderNode {
        let (_trash_outlet, trash_inlet) = connect::<OutputDisposition>(4, None);
        let seek_epoch = seek_obs.epoch();
        let presentation = Presentation::new(
            raw_buffer_chunks,
            PresentationChain::identity(Vec::new()),
            PcmPool::default(),
            initial_spec,
            outlet,
            presentation_cell(seek_epoch).0,
            seek_epoch,
        );
        DecoderNode {
            seek_obs,
            source,
            presentation,
            trash_inlet,
            preload_gate,
            playhead: Arc::new(PlayheadState::new()) as Arc<dyn PlayheadRead>,
            emit: Arc::new(DeferredBus::new(EventBus::new(8), 8)),
            service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
            preload_chunks,
            engine_load: None,
            pending_failure: None,
            runtime: DecoderRuntime {
                seek_epoch,
                ..Default::default()
            },
        }
    }

    #[kithara::test]
    fn decoder_node_preload_progresses_when_raw_capacity_is_below_target() {
        let gate = Arc::new(PreloadGate::default());
        let (outlet, mut inlet) = connect_strict::<Fetch<PresentedPcm>>(3, None);
        let source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Produced(Fetch::data(
                    presentation_block_chunk(),
                    0,
                ))),
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Produced(Fetch::data(
                    presentation_block_chunk(),
                    0,
                ))),
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Produced(Fetch::data(
                    presentation_block_chunk(),
                    0,
                ))),
        )));
        let mut node = test_node_with_limits(
            source,
            outlet,
            Arc::clone(&gate),
            Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
            default_test_spec(),
            1,
            3,
        );

        for admitted in 1..=3 {
            assert_eq!(node.tick(), TickResult::Progress);
            assert_eq!(node.runtime.chunks_sent, admitted);
            assert!(matches!(
                inlet.try_pop(),
                Some(Fetch::Data { data, epoch: 0 })
                    if data.chunk().frames() == PRESENTATION_FRAMES
            ));
            assert_eq!(node.runtime.preloaded, admitted == 3);
            assert_eq!(gate.is_ready(), admitted == 3);
        }
    }

    #[kithara::test]
    fn decoder_node_eof_under_backpressure() {
        let gate = Arc::new(PreloadGate::default());
        let (mut outlet, mut inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);

        outlet.try_push(Fetch::data(presented_chunk(), 0)).unwrap();

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

        assert_eq!(node.tick(), TickResult::Progress);
        assert_eq!(node.tick(), TickResult::Backpressured);
        assert!(!node.runtime.eof_sent);

        let _ = inlet.try_pop();

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.runtime.eof_sent);
        assert!(matches!(inlet.try_pop(), Some(Fetch::NaturalEof { .. })));
    }

    #[kithara::test]
    fn decoder_node_records_engine_load_on_produced() {
        let meter = Arc::new(EngineLoad::default());
        assert!(!meter.snapshot().is_active(), "idle before any tick");

        let (outlet, mut inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);
        let spec = test_spec(2, 44_100);
        let chunk = PcmChunk::new(
            PcmMeta {
                spec,
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

        let mut node = test_node_with_spec(
            source,
            outlet,
            Arc::new(PreloadGate::default()),
            Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
            spec,
        );
        node.engine_load = Some(Arc::clone(&meter));

        assert_eq!(node.tick(), TickResult::Progress);
        let produced = inlet
            .try_pop()
            .expect("presentation commits one output block");
        let Fetch::Data { data, epoch } = produced else {
            panic!("presentation must emit PCM data");
        };
        assert_eq!(epoch, 0);
        assert_eq!(data.chunk().spec(), spec);
        assert_eq!(data.chunk().frames(), PRESENTATION_FRAMES);
        assert!(
            meter.snapshot().is_active(),
            "engine meter records the committed presentation block: {:?}",
            meter.snapshot()
        );
    }

    #[kithara::test]
    fn presentation_failure_is_reported_only_from_the_unchecked_recycle_shell() {
        let failures = Arc::new(AtomicUsize::new(0));
        let seek_state = Arc::new(SeekState::new());
        let source = Box::new(FailureProbeSource {
            failures: Arc::clone(&failures),
            seek_state: Arc::clone(&seek_state),
        });
        let (outlet, _inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);
        let mut node = test_node(
            source,
            outlet,
            Arc::new(PreloadGate::default()),
            seek_state as Arc<dyn SeekObserve>,
        );

        node.fail_presentation("fixture presentation failure");
        assert_eq!(failures.load(Ordering::Relaxed), 0);
        assert!(node.pending_failure.is_some());

        node.recycle();
        assert_eq!(failures.load(Ordering::Relaxed), 1);
        assert!(node.pending_failure.is_none());
    }

    #[kithara::test]
    fn rejected_output_is_released_only_from_the_unchecked_recycle_shell() {
        let (outlet, _inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);
        let mut node = test_node(
            Box::new(Unimock::new(())),
            outlet,
            Arc::new(PreloadGate::default()),
            Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
        );
        let reject_pool = PcmPool::new(1, 0);
        reject_pool.pre_warm(1, |buffer| buffer.resize(PRESENTATION_FRAMES, 0.0));
        let rejected = PcmChunk::new(
            PcmMeta {
                spec: default_test_spec(),
                frames: u32::try_from(PRESENTATION_FRAMES)
                    .expect("presentation frame count fits u32"),
                ..Default::default()
            },
            reject_pool.attach(vec![0.0; PRESENTATION_FRAMES]),
        );

        assert_no_alloc(|| node.presentation.recycle_output(rejected));
        assert_eq!(reject_pool.stats().put_drops, 0);

        node.recycle();

        assert_eq!(reject_pool.stats().put_drops, 1);
    }

    #[kithara::test]
    fn worker_telemetry_throttles_immediate_repeats() {
        let (outlet, _inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);
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

        let mut node = test_node(
            source,
            outlet,
            gate,
            Arc::clone(&seek) as Arc<dyn SeekObserve>,
        );
        node.playhead = Arc::clone(&playhead) as Arc<dyn PlayheadRead>;
        node.emit = Arc::clone(&emit);
        node.engine_load = Some(meter);

        let now = Instant::now();
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
        /// Drains one marker off the outlet.
        /// The two producer terminal steps (`TrackStep::Eof` /
        /// `TrackStep::Failed`) must materialise as distinct variants on
        /// the wire so the consumer can finalise the track only on
        /// natural EOF.
        fn drain_marker<T>(inlet: &mut Inlet<Fetch<T>>) -> Fetch<T> {
            inlet.try_pop().expect("producer pushed a terminal marker")
        }

        let gate = Arc::new(PreloadGate::default());

        let (eof_outlet, mut eof_inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);
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
        let eof_marker = drain_marker(&mut eof_inlet);

        let (failed_outlet, mut failed_inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);
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
        let failed_marker = drain_marker(&mut failed_inlet);

        assert!(matches!(eof_marker, Fetch::NaturalEof { .. }));
        assert!(matches!(failed_marker, Fetch::Failure { .. }));
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
        let gate = Arc::new(PreloadGate::default());
        let (outlet, mut inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);

        // The worker has already sampled the current seek epoch when the
        // consumer requests the next seek concurrently with the source step.
        // The pending EOF still belongs to decode epoch 0.
        let seek_state = Arc::new(SeekState::new());
        let seek_obs = Arc::clone(&seek_state) as Arc<dyn SeekObserve>;
        let source = Box::new(ConcurrentSeekEofSource {
            seek_state: Arc::clone(&seek_state),
        });

        let mut node = test_node(source, outlet, gate, seek_obs);
        assert_eq!(node.tick(), TickResult::Progress);
        assert_eq!(seek_state.epoch(), 1, "source step raced with a new seek");

        let marker = inlet.try_pop().expect("producer pushed an EOF marker");
        assert!(matches!(&marker, Fetch::NaturalEof { .. }));
        assert_eq!(
            marker.epoch(),
            0,
            "EOF marker must carry the producer's decode epoch (0), not the live \
             seek epoch (1) the consumer already advanced"
        );
    }

    #[kithara::test]
    fn decoder_node_preload_gate_waits_for_ring() {
        let gate = Arc::new(PreloadGate::default());
        let (mut outlet, mut inlet) = connect_strict::<Fetch<PresentedPcm>>(2, None);

        outlet.try_push(Fetch::data(presented_chunk(), 0)).unwrap();
        outlet.try_push(Fetch::data(presented_chunk(), 0)).unwrap();

        let source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Produced(Fetch::data(
                    presentation_block_chunk(),
                    0,
                ))),
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Blocked(WaitingReason::Waiting)),
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

        assert_eq!(node.tick(), TickResult::Waiting);
        assert!(!node.runtime.preloaded);
        assert!(!gate.is_ready());

        let _ = inlet.try_pop();

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.runtime.preloaded);
        assert!(gate.is_ready());
        assert!(matches!(
            inlet.try_pop(),
            Some(Fetch::Data { data, epoch: 0 }) if data.chunk().frames() == 1
        ));
        assert!(matches!(
            inlet.try_pop(),
            Some(Fetch::Data { data, epoch: 0 })
                if data.chunk().frames() == PRESENTATION_FRAMES
                    && data.chunk().spec() == default_test_spec()
        ));
    }

    #[kithara::test]
    fn decoder_node_live_upstream_demand_does_not_tick_hang_wait() {
        let gate = Arc::new(PreloadGate::default());
        let (outlet, _inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);

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
        let (outlet, mut inlet) = connect_strict::<Fetch<PresentedPcm>>(1, None);

        let seek_state = Arc::new(SeekState::new());
        let source = Box::new(Unimock::new((
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Produced(Fetch::data(
                    presentation_block_chunk(),
                    0,
                ))),
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::StateChanged),
            MockAudioWorkerSource::step_track
                .next_call(matching!())
                .returns(TrackStep::Produced(Fetch::data(
                    presentation_block_chunk(),
                    1,
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

        node.recycle();
        assert_eq!(node.tick(), TickResult::Progress);
        assert!(!node.runtime.preloaded, "seek resets the preload runtime");
        assert!(!gate.is_ready(), "sync_seek_epoch re-closes the gate");

        assert!(matches!(
            inlet.try_pop(),
            Some(Fetch::Data { data, epoch: 0 })
                if data.chunk().frames() == PRESENTATION_FRAMES
        ));

        assert_eq!(node.tick(), TickResult::Progress);
        assert!(node.runtime.preloaded);
        assert!(gate.is_ready(), "post-seek refill reopens the gate");
        assert!(
            gate.is_ready_for_epoch(epoch),
            "post-seek refill must open the new seek epoch",
        );
        assert!(matches!(
            inlet.try_pop(),
            Some(Fetch::Data { data, epoch: 1 })
                if data.chunk().frames() == PRESENTATION_FRAMES
        ));
    }
}
