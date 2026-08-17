use std::sync::atomic::AtomicU64;

use kithara_abr::AbrHandle;
use kithara_decode::PcmChunk;
use kithara_events::{DeferredBus, Event};
use kithara_platform::{CancelToken, sync::Arc};
use kithara_test_utils::kithara;

use super::{
    AudioWorkerHandle, ConsumerPhase, ConsumerWakeMode, EpochValidator, FailureSource, Fetch,
    Inlet, Outlet, ThreadWake, WakeSignal, connect, cursor::ChunkCursor, event::ReaderOutputWake,
    park::receive_is_nonblocking,
};
use crate::{
    renderer::{OutputDisposition, PresentedPcm},
    runtime::{StrictOutlet, connect_strict},
    traits::{PresentationAdvance, PresentationPoint},
};

enum FetchOutcome {
    Continue,
    Return(Option<PresentedPcm>),
}

pub(super) enum RecvOutcome {
    Closed,
    Empty,
    Item(Fetch<PresentedPcm>),
}

#[derive(Clone, Copy)]
pub(super) struct RecvCtx<'a> {
    pub(super) abr: Option<&'a AbrHandle>,
    pub(super) cancel: Option<&'a CancelToken>,
    pub(super) worker: Option<&'a AudioWorkerHandle>,
}

pub(super) struct RingConsumer {
    pub(super) phase: ConsumerPhase,
    pub(super) validator: EpochValidator,
    pub(super) current_chunk: Option<PresentedPcm>,
    pub(super) preloaded: bool,
    presentation_advance: Option<PresentationAdvance>,
    _epoch: Arc<AtomicU64>,
    reader_wake: Arc<ThreadWake>,
    pcm_rx: Inlet<Fetch<PresentedPcm>>,
    trash_tx: Outlet<OutputDisposition>,
    trash_rejected: Option<OutputDisposition>,
    worker_progress_pending: bool,
    block_on_underrun: bool,
    consumer_wake_mode: ConsumerWakeMode,
}

pub(super) struct RingParts {
    pub(super) epoch: Arc<AtomicU64>,
    pub(super) reader_wake: Arc<ThreadWake>,
    pub(super) pcm_rx: Inlet<Fetch<PresentedPcm>>,
    pub(super) trash_tx: Outlet<OutputDisposition>,
    pub(super) block_on_underrun: bool,
    pub(super) consumer_wake_mode: ConsumerWakeMode,
}

impl RingConsumer {
    pub(super) fn new(parts: RingParts) -> Self {
        let consumer_wake_mode = if parts.block_on_underrun {
            ConsumerWakeMode::ImmediateOffRt
        } else {
            parts.consumer_wake_mode
        };
        Self {
            pcm_rx: parts.pcm_rx,
            validator: EpochValidator::default(),
            phase: ConsumerPhase::Buffering,
            current_chunk: None,
            presentation_advance: None,
            trash_tx: parts.trash_tx,
            trash_rejected: None,
            worker_progress_pending: false,
            reader_wake: parts.reader_wake,
            _epoch: parts.epoch,
            preloaded: false,
            block_on_underrun: parts.block_on_underrun,
            consumer_wake_mode,
        }
    }

    /// Returns whether the worker can progress after the complete seek drain.
    #[kithara::hang_watchdog]
    #[must_use]
    pub(super) fn begin_seek_epoch(&mut self, epoch: u64, cursor: &mut ChunkCursor) -> bool {
        self.validator.epoch = epoch;
        self.presentation_advance = None;
        let mut published = self.recycle_current();
        cursor.clear();
        if self.trash_rejected.is_some() {
            let pending = self.take_worker_progress();
            return published || pending;
        }
        self.phase = ConsumerPhase::SeekPending { epoch };

        let mut popped = false;
        while let Some(fetch) = self.pcm_rx.try_pop() {
            popped = true;
            if fetch.epoch() < epoch {
                if let Fetch::Data { data, .. } = fetch {
                    let discarded = self.discard(data);
                    published |= discarded;
                    if !discarded {
                        break;
                    }
                }
                hang_tick!();
                continue;
            }
            published |= self.stage_post_seek_fetch(fetch, epoch, cursor);
            break;
        }
        let pending = self.take_worker_progress();
        popped || published || pending
    }

    pub(super) fn wake_worker(&self, worker: Option<&AudioWorkerHandle>) {
        wake_worker(worker, self.consumer_wake_mode);
    }

    fn consumer_hang_ctx(&self, ctx: RecvCtx<'_>) -> ConsumerHangCtx {
        ConsumerHangCtx {
            phase: format!("{:?}", self.phase),
            variant: ctx.abr.and_then(AbrHandle::current_variant_index),
            abr_escaping: ctx.abr.map(AbrHandle::is_escaping),
            abr_locked: ctx.abr.map(AbrHandle::is_locked),
            abr_pending: ctx
                .abr
                .and_then(AbrHandle::peek_pending_decision)
                .map(|decision| format!("{decision:?}")),
            epoch: self.validator.epoch,
            preloaded: self.preloaded,
            block_on_underrun: self.block_on_underrun,
        }
    }

    fn discard(&mut self, presented: PresentedPcm) -> bool {
        if self.trash_rejected.is_some() {
            debug_assert!(self.current_chunk.is_none());
            self.current_chunk = Some(presented);
            self.fail_output_return();
            return false;
        }
        if let Some(rejected) = self.push_output(presented.returned()) {
            self.trash_rejected = Some(rejected);
            self.fail_output_return();
            return false;
        }
        self.worker_progress_pending = true;
        true
    }

    pub(super) fn detach(
        &mut self,
        presented: PresentedPcm,
    ) -> Result<PcmChunk, super::DecodeError> {
        if self.trash_rejected.is_some() {
            self.current_chunk = Some(presented);
            self.fail_output_return();
            return Err(output_return_error());
        }
        let point = presented.point();
        let (chunk, disposition) = presented.detach();
        if self.push_output(disposition).is_some() {
            self.current_chunk = Some(PresentedPcm::new(chunk, point));
            self.fail_output_return();
            return Err(output_return_error());
        }
        self.worker_progress_pending = true;
        Ok(chunk)
    }

    fn push_output(&mut self, disposition: OutputDisposition) -> Option<OutputDisposition> {
        self.trash_tx.try_push(disposition).err()
    }

    fn fail_output_return(&mut self) {
        self.phase = ConsumerPhase::Failed {
            source: FailureSource::Producer,
        };
    }

    /// Takes the coalesced final-output publication signal.
    pub(super) fn take_worker_progress(&mut self) -> bool {
        let pending = self.worker_progress_pending;
        self.worker_progress_pending = false;
        pending
    }

    pub(super) fn fill(&mut self, cursor: &mut ChunkCursor, ctx: RecvCtx<'_>) -> bool {
        let Some(chunk) = self.recv_valid_chunk(ctx) else {
            return false;
        };
        cursor.begin_chunk(chunk.chunk());
        self.current_chunk = Some(chunk);
        self.promote_playing();
        true
    }

    fn process_fetch(&mut self, fetch: Fetch<PresentedPcm>) -> FetchOutcome {
        if !self.validator.is_valid(&fetch) {
            if let Fetch::Data { data, .. } = fetch
                && !self.discard(data)
            {
                return FetchOutcome::Return(None);
            }
            return FetchOutcome::Continue;
        }

        match fetch {
            Fetch::NaturalEof { .. } => {
                self.phase = ConsumerPhase::AtEof;
                FetchOutcome::Return(None)
            }
            Fetch::Failure { .. } => {
                self.phase = ConsumerPhase::Failed {
                    source: FailureSource::Producer,
                };
                FetchOutcome::Return(None)
            }
            Fetch::Data { data, epoch } => {
                if data.point().seek_epoch() != epoch {
                    self.discard(data);
                    self.phase = ConsumerPhase::Failed {
                        source: FailureSource::Producer,
                    };
                    FetchOutcome::Return(None)
                } else {
                    FetchOutcome::Return(Some(data))
                }
            }
        }
    }

    pub(super) const fn promote_playing(&mut self) {
        if matches!(
            self.phase,
            ConsumerPhase::Buffering | ConsumerPhase::SeekPending { .. }
        ) {
            self.phase = ConsumerPhase::Playing;
        }
    }

    pub(super) fn recv_outcome(&mut self, ctx: RecvCtx<'_>) -> RecvOutcome {
        if receive_is_nonblocking(self.preloaded, self.block_on_underrun) {
            if let Some(fetch) =
                try_pop_and_wake(&mut self.pcm_rx, ctx.worker, self.consumer_wake_mode)
            {
                return RecvOutcome::Item(fetch);
            }
            return RecvOutcome::Empty;
        }
        self.recv_outcome_blocking(ctx)
    }

    #[kithara::flash(true)]
    #[kithara::hang_watchdog(ctx = ConsumerHangCtx)]
    fn recv_outcome_blocking(&mut self, ctx: RecvCtx<'_>) -> RecvOutcome {
        loop {
            if let Some(fetch) =
                try_pop_and_wake(&mut self.pcm_rx, ctx.worker, self.consumer_wake_mode)
            {
                hang_reset!();
                return RecvOutcome::Item(fetch);
            }
            if ctx.cancel.is_some_and(CancelToken::is_cancelled) {
                hang_reset!();
                return RecvOutcome::Closed;
            }
            wake_worker(ctx.worker, self.consumer_wake_mode);
            let since = self.reader_wake.current();
            if let Some(fetch) =
                try_pop_and_wake(&mut self.pcm_rx, ctx.worker, self.consumer_wake_mode)
            {
                hang_reset!();
                return RecvOutcome::Item(fetch);
            }
            if ctx.cancel.is_some_and(CancelToken::is_cancelled) {
                hang_reset!();
                return RecvOutcome::Closed;
            }
            hang_park!(
                |remaining| {
                    self.reader_wake.wait_timeout(since, remaining);
                },
                self.consumer_hang_ctx(ctx)
            );
        }
    }

    #[kithara::hang_watchdog]
    pub(super) fn recv_valid_chunk(&mut self, ctx: RecvCtx<'_>) -> Option<PresentedPcm> {
        if self.phase.is_terminal() {
            return None;
        }

        loop {
            match self.recv_outcome(ctx) {
                RecvOutcome::Item(fetch) => match self.process_fetch(fetch) {
                    FetchOutcome::Continue => {
                        hang_tick!();
                    }
                    FetchOutcome::Return(chunk) => {
                        hang_reset!();
                        return chunk;
                    }
                },
                RecvOutcome::Empty => return None,
                RecvOutcome::Closed => {
                    hang_reset!();
                    self.phase = ConsumerPhase::Failed {
                        source: FailureSource::ChannelClosed,
                    };
                    return None;
                }
            }
        }
    }

    #[must_use]
    pub(super) fn recycle_current(&mut self) -> bool {
        if let Some(chunk) = self.current_chunk.take() {
            return self.discard(chunk);
        }
        false
    }

    pub(super) fn record_presentation_advance(
        &mut self,
        point: PresentationPoint,
        read_offset_frames: usize,
    ) {
        self.presentation_advance = Some(PresentationAdvance::new(point, read_offset_frames));
    }

    pub(super) fn take_presentation_advance(&mut self) -> Option<PresentationAdvance> {
        self.presentation_advance.take()
    }

    fn stage_post_seek_fetch(
        &mut self,
        fetch: Fetch<PresentedPcm>,
        epoch: u64,
        cursor: &mut ChunkCursor,
    ) -> bool {
        debug_assert_eq!(
            fetch.epoch(),
            epoch,
            "PCM ring preserved a fetch from a future seek epoch"
        );
        match fetch {
            Fetch::Data { data, .. } => {
                if data.point().seek_epoch() != epoch {
                    let published = self.discard(data);
                    self.phase = ConsumerPhase::Failed {
                        source: FailureSource::ProducerAfterSeek,
                    };
                    published
                } else {
                    cursor.begin_chunk(data.chunk());
                    self.current_chunk = Some(data);
                    self.phase = ConsumerPhase::Playing;
                    false
                }
            }
            Fetch::NaturalEof { .. } => {
                self.phase = ConsumerPhase::AtEof;
                false
            }
            Fetch::Failure { .. } => {
                self.phase = ConsumerPhase::Failed {
                    source: FailureSource::ProducerAfterSeek,
                };
                false
            }
        }
    }
}

pub(super) fn create_channels(
    capacity: usize,
    emit: Arc<DeferredBus<Event>>,
    reader_wake: &Arc<ThreadWake>,
) -> (
    StrictOutlet<Fetch<PresentedPcm>>,
    Inlet<Fetch<PresentedPcm>>,
) {
    let wake: Arc<dyn WakeSignal> = Arc::new(ReaderOutputWake::new(Arc::clone(reader_wake), emit));
    connect_strict::<Fetch<PresentedPcm>>(capacity.max(1), Some(wake))
}

pub(super) fn create_trash_channel(
    pcm_buffer_chunks: usize,
) -> (Outlet<OutputDisposition>, Inlet<OutputDisposition>) {
    connect::<OutputDisposition>(pcm_buffer_chunks.max(1) + 2, None)
}

#[derive(serde::Serialize)]
struct ConsumerHangCtx {
    abr_escaping: Option<bool>,
    abr_locked: Option<bool>,
    abr_pending: Option<String>,
    variant: Option<usize>,
    phase: String,
    block_on_underrun: bool,
    preloaded: bool,
    epoch: u64,
}

fn try_pop_and_wake(
    pcm_rx: &mut Inlet<Fetch<PresentedPcm>>,
    worker: Option<&AudioWorkerHandle>,
    mode: ConsumerWakeMode,
) -> Option<Fetch<PresentedPcm>> {
    let fetch = pcm_rx.try_pop()?;
    wake_worker(worker, mode);
    Some(fetch)
}

fn wake_worker(worker: Option<&AudioWorkerHandle>, mode: ConsumerWakeMode) {
    let Some(worker) = worker else {
        return;
    };
    match mode {
        ConsumerWakeMode::RealtimeDeferred => worker.defer_wake(),
        ConsumerWakeMode::ImmediateOffRt => worker.wake(),
    }
}

const fn output_return_error() -> super::DecodeError {
    super::DecodeError::InvalidData {
        detail: "bounded presentation output return ring is full",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmMeta};
    use kithara_platform::{CancelToken, sync::Arc};
    use kithara_stream::PlayheadState;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{ConsumerWakeMode, audio::ReadOutcome};

    struct RingFixture {
        playhead: Arc<PlayheadState>,
        events: crate::audio::event::AudioEvents,
        cursor: ChunkCursor,
        _trash_rx: Inlet<OutputDisposition>,
        data_tx: Outlet<Fetch<PresentedPcm>>,
        ring: RingConsumer,
    }

    impl RingFixture {
        fn new(preloaded: bool) -> Self {
            Self::with_wake_mode(preloaded, false, ConsumerWakeMode::RealtimeDeferred)
        }

        fn with_wake_mode(
            preloaded: bool,
            block_on_underrun: bool,
            consumer_wake_mode: ConsumerWakeMode,
        ) -> Self {
            let (data_tx, pcm_rx) = connect::<Fetch<PresentedPcm>>(4, None);
            let (trash_tx, trash_rx) = connect::<OutputDisposition>(8, None);
            let pool = PcmPool::default();
            let mut ring = RingConsumer::new(RingParts {
                pcm_rx,
                trash_tx,
                reader_wake: Arc::new(ThreadWake::default()),
                epoch: Arc::new(AtomicU64::new(0)),
                block_on_underrun,
                consumer_wake_mode,
            });
            ring.preloaded = preloaded;
            Self {
                ring,
                data_tx,
                cursor: ChunkCursor::new(&pool, PcmMeta::default().spec),
                events: crate::audio::event::AudioEvents::test(),
                playhead: Arc::new(PlayheadState::new()),
                _trash_rx: trash_rx,
            }
        }

        fn recv(&mut self) -> Option<PcmChunk> {
            self.ring.recv_valid_chunk(empty_ctx()).map(PcmChunk::from)
        }
    }

    fn empty_ctx() -> RecvCtx<'static> {
        RecvCtx {
            cancel: None,
            worker: None,
            abr: None,
        }
    }

    fn make_presented(samples: &[f32], epoch: u64) -> PresentedPcm {
        let mut meta = PcmMeta::default();
        meta.spec.channels = 1;
        meta.frames = u32::try_from(samples.len()).unwrap_or(u32::MAX);
        let frame = u64::from(meta.frames);
        let rate = meta.spec.sample_rate;
        PresentedPcm::new(
            PcmChunk::new(meta, PcmPool::default().attach(samples.to_vec())),
            PresentationPoint::new(epoch, frame, 0, frame, rate),
        )
    }

    #[kithara::test]
    fn block_on_underrun_forces_immediate_off_rt_wakes() {
        let fixture = RingFixture::with_wake_mode(false, true, ConsumerWakeMode::RealtimeDeferred);

        assert_eq!(
            fixture.ring.consumer_wake_mode,
            ConsumerWakeMode::ImmediateOffRt
        );
    }

    #[kithara::test]
    fn explicit_off_rt_mode_is_immediate_without_blocking_reads() {
        let mut fixture =
            RingFixture::with_wake_mode(true, false, ConsumerWakeMode::ImmediateOffRt);

        assert_eq!(
            fixture.ring.consumer_wake_mode,
            ConsumerWakeMode::ImmediateOffRt
        );
        assert!(matches!(
            fixture.ring.recv_outcome(empty_ctx()),
            RecvOutcome::Empty
        ));
    }

    #[kithara::test]
    fn saturated_return_ring_retains_every_output() {
        let mut fixture = RingFixture::new(true);
        let (trash_tx, trash_rx) = connect::<OutputDisposition>(1, None);
        fixture.ring.trash_tx = trash_tx;
        fixture._trash_rx = trash_rx;

        assert!(fixture.ring.discard(make_presented(&[0.1], 0)));
        assert!(fixture.ring.discard(make_presented(&[0.2], 0)));
        assert!(!fixture.ring.discard(make_presented(&[0.3], 0)));
        assert!(!fixture.ring.discard(make_presented(&[0.4], 0)));

        assert!(matches!(
            fixture._trash_rx.try_pop(),
            Some(OutputDisposition::Returned(_))
        ));
        assert!(fixture.ring.trash_tx.flush());
        assert!(matches!(
            fixture._trash_rx.try_pop(),
            Some(OutputDisposition::Returned(_))
        ));
        assert!(fixture._trash_rx.try_pop().is_none());
        assert!(matches!(
            fixture.ring.trash_rejected.as_ref(),
            Some(OutputDisposition::Returned(_))
        ));
        assert!(fixture.ring.current_chunk.is_some());
        assert!(matches!(
            fixture.ring.phase,
            ConsumerPhase::Failed {
                source: FailureSource::Producer
            }
        ));
    }

    #[kithara::test]
    fn seek_drain_reports_whether_it_popped_any_item() {
        let mut drained = RingFixture::new(true);
        drained
            .data_tx
            .try_push(Fetch::data(make_presented(&[0.1], 0), 0))
            .expect("first stale chunk reaches ring");
        drained
            .data_tx
            .try_push(Fetch::data(make_presented(&[0.2], 0), 0))
            .expect("second stale chunk reaches ring");
        drained
            .data_tx
            .try_push(Fetch::eof(1))
            .expect("current epoch marker reaches ring");

        assert!(drained.ring.begin_seek_epoch(1, &mut drained.cursor));

        let mut empty = RingFixture::new(true);
        assert!(!empty.ring.begin_seek_epoch(1, &mut empty.cursor));
    }

    #[kithara::test]
    fn current_only_seek_reports_return_progress() {
        let mut fixture = RingFixture::new(true);
        fixture.ring.current_chunk = Some(make_presented(&[0.1, 0.2], 0));

        assert!(fixture.ring.begin_seek_epoch(1, &mut fixture.cursor));
        assert!(fixture.ring.current_chunk.is_none());
        assert!(matches!(
            fixture._trash_rx.try_pop(),
            Some(OutputDisposition::Returned(_))
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
    #[should_panic(expected = "recv_outcome_blocking")]
    fn blocking_recv_without_preload_panics_when_no_chunk_arrives() {
        let mut fixture = RingFixture::new(false);
        let _chunk = fixture.recv();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn blocking_recv_returns_closed_after_cancel() {
        let mut fixture = RingFixture::new(false);
        let cancel = CancelToken::never();
        cancel.cancel();
        assert!(matches!(
            fixture.ring.recv_outcome(RecvCtx {
                cancel: Some(&cancel),
                worker: None,
                abr: None,
            }),
            RecvOutcome::Closed
        ));
    }

    #[kithara::test]
    fn preloaded_recv_is_nonblocking() {
        let mut fixture = RingFixture::new(true);
        assert!(matches!(
            fixture.ring.recv_outcome(empty_ctx()),
            RecvOutcome::Empty
        ));
    }

    #[kithara::test]
    fn consumer_phase_starts_buffering() {
        let fixture = RingFixture::new(true);
        assert_eq!(fixture.ring.phase, ConsumerPhase::Buffering);
    }

    #[kithara::test]
    fn consumer_phase_transitions_to_playing_on_first_chunk() {
        let mut fixture = RingFixture::new(true);
        fixture
            .data_tx
            .try_push(Fetch::data(make_presented(&[0.1, 0.2], 0), 0))
            .expect("chunk reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));
        assert_eq!(fixture.ring.phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn consumer_phase_transitions_to_seek_pending() {
        let mut fixture = RingFixture::new(true);
        let _ = fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);
        assert!(matches!(
            fixture.ring.phase,
            ConsumerPhase::SeekPending { .. }
        ));
    }

    #[kithara::test]
    fn consumer_phase_seek_pending_to_playing_on_chunk() {
        let mut fixture = RingFixture::new(true);
        let _ = fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);
        fixture
            .data_tx
            .try_push(Fetch::data(make_presented(&[0.1, 0.2], 1), 1))
            .expect("post-seek chunk reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));
        assert_eq!(fixture.ring.phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn seek_drain_preserves_new_epoch_chunk_after_stale_chunks() {
        let mut fixture = RingFixture::new(true);
        fixture
            .data_tx
            .try_push(Fetch::data(make_presented(&[0.1, 0.2], 0), 0))
            .expect("stale chunk reaches ring");
        fixture
            .data_tx
            .try_push(Fetch::data(make_presented(&[0.7, 0.8], 1), 1))
            .expect("fresh chunk reaches ring");
        let _ = fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);
        let mut buf = [0.0; 2];
        let read = fixture
            .cursor
            .read(
                &mut fixture.ring,
                &mut fixture.events,
                fixture.playhead.as_ref(),
                empty_ctx(),
                &mut buf,
            )
            .expect("post-seek read succeeds");
        let ReadOutcome::Frames { count, .. } = read.outcome else {
            panic!("expected preserved post-seek frames");
        };
        assert_eq!(count.get(), 2);
        assert_eq!(buf, [0.7, 0.8]);
    }

    #[kithara::test]
    fn seek_drain_preserves_new_epoch_eof_after_stale_chunks() {
        let mut fixture = RingFixture::new(true);
        fixture
            .data_tx
            .try_push(Fetch::data(make_presented(&[0.1, 0.2], 0), 0))
            .expect("stale chunk reaches ring");
        fixture
            .data_tx
            .try_push(Fetch::eof(1))
            .expect("eof reaches ring");
        let _ = fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);
        let mut buf = [0.0; 2];
        let read = fixture
            .cursor
            .read(
                &mut fixture.ring,
                &mut fixture.events,
                fixture.playhead.as_ref(),
                empty_ctx(),
                &mut buf,
            )
            .expect("post-seek eof read succeeds");
        assert!(matches!(read.outcome, ReadOutcome::Eof { .. }));
        assert_eq!(fixture.ring.phase, ConsumerPhase::AtEof);
    }

    #[kithara::test]
    fn consumer_phase_eof_terminates() {
        let mut fixture = RingFixture::new(true);
        fixture
            .data_tx
            .try_push(Fetch::eof(0))
            .expect("eof reaches ring");
        assert!(fixture.recv().is_none());
        assert_eq!(fixture.ring.phase, ConsumerPhase::AtEof);
    }

    #[kithara::test]
    fn consumer_phase_failed_on_channel_close() {
        let mut fixture = RingFixture::new(false);
        let cancel = CancelToken::never();
        cancel.cancel();
        assert!(
            fixture
                .ring
                .recv_valid_chunk(RecvCtx {
                    cancel: Some(&cancel),
                    worker: None,
                    abr: None,
                })
                .is_none()
        );
        assert_eq!(
            fixture.ring.phase,
            ConsumerPhase::Failed {
                source: FailureSource::ChannelClosed
            }
        );
    }

    #[kithara::test]
    fn consumer_does_not_park_in_terminal_phase() {
        let mut fixture = RingFixture::new(false);
        fixture.ring.phase = ConsumerPhase::AtEof;
        assert!(fixture.recv().is_none());
    }

    #[kithara::test]
    fn process_fetch_must_distinguish_failure_from_natural_eof() {
        let mut eof = RingFixture::new(true);
        eof.data_tx
            .try_push(Fetch::eof(0))
            .expect("natural eof reaches ring");
        let _chunk = eof.recv();
        assert_eq!(eof.ring.phase, ConsumerPhase::AtEof);

        let mut failed = RingFixture::new(true);
        failed
            .data_tx
            .try_push(Fetch::failure(0))
            .expect("failure reaches ring");
        let _chunk = failed.recv();
        assert_ne!(failed.ring.phase, ConsumerPhase::AtEof);
        assert_eq!(
            failed.ring.phase,
            ConsumerPhase::Failed {
                source: FailureSource::Producer
            }
        );
    }
}
