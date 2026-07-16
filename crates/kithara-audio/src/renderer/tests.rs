use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_events::{DeferredBus, Event, EventBus};
use kithara_platform::{CancelToken, sync::Arc};
use kithara_stream::{PlayheadRead, PlayheadState, SeekControl, SeekObserve, SeekState};
use kithara_test_utils::kithara;

use super::{AudioWorkerHandle, AudioWorkerSource, DecoderNode, PreloadGate, TrackRegistration};
use crate::{
    pipeline::{
        fetch::Fetch,
        track::{TrackStep, WaitingReason},
    },
    runtime::{AtomicServiceClass, Node, Outlet, ServiceClass, TickResult, connect},
    source_audio::{
        SourceAudioDemand, SourceAudioError, SourceAudioReadOutcome, SourceAudioReader,
        SourceAudioTap, SourceAudioTerminal, SourceFrameRange, connect_source_audio,
    },
};

fn empty_chunk() -> PcmChunk {
    PcmChunk::new(PcmMeta::default(), PcmPool::default().attach(Vec::new()))
}

pub(crate) struct MockSource {
    pub(crate) seek: Arc<dyn SeekControl>,
    seek_obs: Arc<dyn SeekObserve>,
    ready: bool,
    should_panic: bool,
    chunks_to_produce: usize,
    cursor: usize,
}

impl MockSource {
    pub(crate) fn new(chunks: usize) -> Self {
        let state = Arc::new(SeekState::new());
        let seek = Arc::clone(&state) as Arc<dyn SeekControl>;
        let seek_obs = Arc::clone(&state) as Arc<dyn SeekObserve>;
        Self {
            seek,
            seek_obs,
            chunks_to_produce: chunks,
            cursor: 0,
            ready: true,
            should_panic: false,
        }
    }

    pub(crate) fn not_ready(chunks: usize) -> Self {
        Self {
            ready: false,
            ..Self::new(chunks)
        }
    }

    pub(crate) fn panicking() -> Self {
        Self {
            should_panic: true,
            ..Self::new(100)
        }
    }
}

impl AudioWorkerSource for MockSource {
    type Chunk = PcmChunk;

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek_obs)
    }

    fn step_track(&mut self) -> TrackStep<PcmChunk> {
        if self.seek_obs.is_pending() || self.seek_obs.is_flushing() {
            let epoch = self.seek_obs.epoch();
            self.seek.complete(epoch);
            self.seek.clear_pending(epoch);
            return TrackStep::StateChanged;
        }
        if !self.ready {
            return TrackStep::Blocked(WaitingReason::Waiting);
        }
        if self.should_panic {
            panic!("mock panic for testing");
        }
        if self.cursor >= self.chunks_to_produce {
            return TrackStep::Eof;
        }
        self.cursor += 1;
        TrackStep::Produced(Fetch::data(empty_chunk(), 0))
    }
}

#[derive(Clone, Copy)]
enum AuthoritativeStep {
    Progress,
    Eof,
    Failed,
}

struct AuthoritativeSource {
    tap: SourceAudioTap,
    seek: Arc<SeekState>,
    step: AuthoritativeStep,
}

impl AudioWorkerSource for AuthoritativeSource {
    type Chunk = PcmChunk;

    fn decode_epoch(&self) -> u64 {
        7
    }

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    fn source_audio_ready(&mut self) -> bool {
        self.tap.service().expect("source audio service");
        self.tap.can_step()
    }

    fn processed_output_required(&self) -> bool {
        !self.tap.is_authoritative()
    }

    fn finish_source_audio_eof(&mut self, decode_seek_epoch: u64) -> bool {
        self.tap.finish(decode_seek_epoch, SourceAudioTerminal::Eof)
    }

    fn finish_source_audio_failed(&mut self, decode_seek_epoch: u64) -> bool {
        self.tap
            .finish(decode_seek_epoch, SourceAudioTerminal::Failed)
    }

    fn step_track(&mut self) -> TrackStep<PcmChunk> {
        match self.step {
            AuthoritativeStep::Progress => TrackStep::StateChanged,
            AuthoritativeStep::Eof => TrackStep::Eof,
            AuthoritativeStep::Failed => TrackStep::Failed,
        }
    }
}

fn authoritative_source(
    step: AuthoritativeStep,
) -> (
    AuthoritativeSource,
    SourceAudioReader,
    SourceAudioDemand,
    AudioWorkerHandle,
) {
    let worker = AudioWorkerHandle::with_cancel(CancelToken::never());
    let (mut reader, mut tap) = connect_source_audio(
        &PcmPool::default(),
        NonZeroUsize::new(2).expect("non-zero source capacity"),
        NonZeroUsize::new(2).expect("non-zero source window"),
        worker.clone(),
    )
    .expect("source audio connection");
    let spec = PcmSpec::new(1, NonZeroU32::new(48_000).expect("static sample rate"));
    reader
        .activate_authoritative(spec)
        .expect("authoritative activation");
    tap.service().expect("install activation");
    let demand = reader
        .request(SourceFrameRange::new(0, 2).expect("source range"), 0, 7)
        .expect("source demand");
    tap.service().expect("install demand");
    (
        AuthoritativeSource {
            tap,
            seek: Arc::new(SeekState::new()),
            step,
        },
        reader,
        demand,
        worker,
    )
}

fn decoder_node(source: AuthoritativeSource, outlet: Outlet<Fetch<PcmChunk>>) -> DecoderNode {
    let (_trash_outlet, trash_inlet) = connect::<PcmChunk>(2, None);
    TrackRegistration {
        preload_gate: Arc::new(PreloadGate::default()),
        service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
        source: Box::new(source),
        trash_inlet,
        playhead: Arc::new(PlayheadState::new()) as Arc<dyn PlayheadRead>,
        emit: Arc::new(DeferredBus::<Event>::new(EventBus::new(8), 8)),
        engine_load: None,
        outlet,
        preload_chunks: 1,
    }
    .into()
}

#[kithara::test]
fn authoritative_progress_ignores_full_processed_output_outlet() {
    let (source, _reader, _demand, worker) = authoritative_source(AuthoritativeStep::Progress);
    let (mut outlet, _inlet) = connect::<Fetch<PcmChunk>>(1, None);
    outlet
        .try_push(Fetch::data(empty_chunk(), 0))
        .expect("fill processed output ring");
    outlet
        .try_push(Fetch::data(empty_chunk(), 0))
        .expect("fill processed output overflow");
    assert!(outlet.has_pending());
    let mut node = decoder_node(source, outlet);

    assert_eq!(node.tick(), TickResult::Progress);
    worker.shutdown();
}

#[kithara::test]
fn authoritative_eof_reaches_source_audio_reader() {
    let (source, mut reader, demand, worker) = authoritative_source(AuthoritativeStep::Eof);
    let (outlet, _inlet) = connect::<Fetch<PcmChunk>>(1, None);
    let mut node = decoder_node(source, outlet);

    assert_eq!(node.tick(), TickResult::Progress);
    let mut output = [0.0; 2];
    assert_eq!(
        reader.read_into(&demand, &mut output),
        Ok(SourceAudioReadOutcome::Eof)
    );
    worker.shutdown();
}

#[kithara::test]
fn authoritative_failure_reaches_source_audio_reader() {
    let (source, mut reader, demand, worker) = authoritative_source(AuthoritativeStep::Failed);
    let (outlet, _inlet) = connect::<Fetch<PcmChunk>>(1, None);
    let mut node = decoder_node(source, outlet);

    assert_eq!(node.tick(), TickResult::Done);
    let mut output = [0.0; 2];
    assert_eq!(
        reader.read_into(&demand, &mut output),
        Err(SourceAudioError::SourceFailed)
    );
    worker.shutdown();
}
