use std::sync::atomic::AtomicU64;

use kithara_abr::AbrHandle;
use kithara_decode::PcmChunk;
use kithara_events::{DeferredBus, Event};
use kithara_platform::{CancelToken, sync::Arc};
use kithara_test_utils::kithara;

use super::{
    AudioWorkerHandle, ConsumerPhase, EpochValidator, Fetch, Inlet, Outlet, ThreadWake, WakeSignal,
    connect,
    cursor::ChunkCursor,
    event::ReaderOutputWake,
    park::{receive_is_nonblocking, wait_for_fetch},
};

enum FetchOutcome {
    Continue,
    Return(Option<PcmChunk>),
}

pub(super) enum RecvOutcome {
    Closed,
    Empty,
    Item(Fetch<PcmChunk>),
}

#[derive(Clone, Copy)]
pub(super) enum Lookahead {
    Available,
    Eof,
    Pending,
}

#[derive(Clone, Copy)]
pub(super) struct RecvCtx<'a> {
    pub(super) cancel: Option<&'a CancelToken>,
    pub(super) worker: Option<&'a AudioWorkerHandle>,
    pub(super) abr: Option<&'a AbrHandle>,
}

pub(super) struct RingConsumer {
    pcm_rx: Inlet<Fetch<PcmChunk>>,
    pub(super) validator: EpochValidator,
    pub(super) phase: ConsumerPhase,
    pub(super) current_chunk: Option<PcmChunk>,
    pub(super) lookahead: Option<PcmChunk>,
    trash_tx: Outlet<PcmChunk>,
    reader_wake: Arc<ThreadWake>,
    _epoch: Arc<AtomicU64>,
    pub(super) preloaded: bool,
    block_on_underrun: bool,
}

pub(super) struct RingParts {
    pub(super) pcm_rx: Inlet<Fetch<PcmChunk>>,
    pub(super) trash_tx: Outlet<PcmChunk>,
    pub(super) reader_wake: Arc<ThreadWake>,
    pub(super) epoch: Arc<AtomicU64>,
    pub(super) block_on_underrun: bool,
}

impl RingConsumer {
    pub(super) fn new(parts: RingParts) -> Self {
        Self {
            pcm_rx: parts.pcm_rx,
            validator: EpochValidator::default(),
            phase: ConsumerPhase::Buffering,
            current_chunk: None,
            lookahead: None,
            trash_tx: parts.trash_tx,
            reader_wake: parts.reader_wake,
            _epoch: parts.epoch,
            preloaded: false,
            block_on_underrun: parts.block_on_underrun,
        }
    }

    pub(super) fn fill(&mut self, cursor: &mut ChunkCursor, ctx: RecvCtx<'_>) -> bool {
        let Some(chunk) = self.take_buffered().or_else(|| self.recv_valid_chunk(ctx)) else {
            return false;
        };
        cursor.begin_chunk(&chunk);
        self.current_chunk = Some(chunk);
        self.promote_playing();
        true
    }

    pub(super) fn ensure_lookahead(&mut self, ctx: RecvCtx<'_>) -> Lookahead {
        if self.lookahead.is_some() {
            return Lookahead::Available;
        }

        match self.recv_valid_chunk(ctx) {
            Some(chunk) => {
                self.lookahead = Some(chunk);
                Lookahead::Available
            }
            None if self.phase == ConsumerPhase::AtEof => Lookahead::Eof,
            None => Lookahead::Pending,
        }
    }

    pub(super) fn take_buffered(&mut self) -> Option<PcmChunk> {
        self.current_chunk.take().or_else(|| self.lookahead.take())
    }

    #[kithara::hang_watchdog]
    pub(super) fn recv_valid_chunk(&mut self, ctx: RecvCtx<'_>) -> Option<PcmChunk> {
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
                    self.phase = ConsumerPhase::Failed;
                    return None;
                }
            }
        }
    }

    pub(super) fn recv_outcome(&mut self, ctx: RecvCtx<'_>) -> RecvOutcome {
        if receive_is_nonblocking(self.preloaded, self.block_on_underrun) {
            if let Some(fetch) = self.pcm_rx.try_pop() {
                wake_worker(ctx.worker);
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
            if let Some(fetch) = self.pcm_rx.try_pop() {
                hang_reset!();
                wake_worker(ctx.worker);
                return RecvOutcome::Item(fetch);
            }
            if ctx.cancel.is_some_and(CancelToken::is_cancelled) {
                hang_reset!();
                return RecvOutcome::Closed;
            }
            wake_worker(ctx.worker);
            self.reader_wake.register_current();
            if let Some(fetch) = self.pcm_rx.try_pop() {
                hang_reset!();
                wake_worker(ctx.worker);
                return RecvOutcome::Item(fetch);
            }
            if ctx.cancel.is_some_and(CancelToken::is_cancelled) {
                hang_reset!();
                return RecvOutcome::Closed;
            }
            hang_park!(wait_for_fetch, self.consumer_hang_ctx(ctx));
        }
    }

    pub(super) fn recycle_current(&mut self) {
        if let Some(chunk) = self.current_chunk.take() {
            self.discard(chunk);
        }
    }

    pub(super) fn recycle_lookahead(&mut self) {
        if let Some(chunk) = self.lookahead.take() {
            self.discard(chunk);
        }
    }

    pub(super) fn discard(&mut self, chunk: PcmChunk) {
        if let Err(_overflow) = self.trash_tx.try_push(chunk) {
            debug_assert!(
                false,
                "PCM trash ring overflow - spent buffer freed on the audio thread"
            );
        }
    }

    #[kithara::hang_watchdog]
    pub(super) fn begin_seek_epoch(&mut self, epoch: u64, cursor: &mut ChunkCursor) {
        self.validator.epoch = epoch;
        self.recycle_current();
        self.recycle_lookahead();
        cursor.clear();
        self.phase = ConsumerPhase::SeekPending { epoch };

        while let Some(fetch) = self.pcm_rx.try_pop() {
            if fetch.epoch() < epoch {
                if let Fetch::Data { data, .. } = fetch {
                    self.discard(data);
                }
                hang_tick!();
                continue;
            }
            self.stage_post_seek_fetch(fetch, epoch, cursor);
            break;
        }
    }

    fn process_fetch(&mut self, fetch: Fetch<PcmChunk>) -> FetchOutcome {
        if !self.validator.is_valid(&fetch) {
            if let Fetch::Data { data, .. } = fetch {
                self.discard(data);
            }
            return FetchOutcome::Continue;
        }

        match fetch {
            Fetch::NaturalEof { .. } => {
                self.phase = ConsumerPhase::AtEof;
                FetchOutcome::Return(None)
            }
            Fetch::Failure { .. } => {
                self.phase = ConsumerPhase::Failed;
                FetchOutcome::Return(None)
            }
            Fetch::Data { data, .. } => FetchOutcome::Return(Some(data)),
        }
    }

    fn stage_post_seek_fetch(
        &mut self,
        fetch: Fetch<PcmChunk>,
        epoch: u64,
        cursor: &mut ChunkCursor,
    ) {
        debug_assert_eq!(
            fetch.epoch(),
            epoch,
            "PCM ring preserved a fetch from a future seek epoch"
        );
        match fetch {
            Fetch::Data { data, .. } => {
                cursor.begin_chunk(&data);
                self.current_chunk = Some(data);
                self.phase = ConsumerPhase::Playing;
            }
            Fetch::NaturalEof { .. } => {
                self.phase = ConsumerPhase::AtEof;
            }
            Fetch::Failure { .. } => {
                self.phase = ConsumerPhase::Failed;
            }
        }
    }

    pub(super) fn promote_playing(&mut self) {
        if matches!(
            self.phase,
            ConsumerPhase::Buffering | ConsumerPhase::SeekPending { .. }
        ) {
            self.phase = ConsumerPhase::Playing;
        }
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
}

pub(super) fn create_channels(
    pcm_buffer_chunks: usize,
    emit: Arc<DeferredBus<Event>>,
    reader_wake: &Arc<ThreadWake>,
) -> (Outlet<Fetch<PcmChunk>>, Inlet<Fetch<PcmChunk>>) {
    let wake: Arc<dyn WakeSignal> = Arc::new(ReaderOutputWake::new(Arc::clone(reader_wake), emit));
    connect::<Fetch<PcmChunk>>(pcm_buffer_chunks.max(1), Some(wake))
}

pub(super) fn create_trash_channel(
    pcm_buffer_chunks: usize,
) -> (Outlet<PcmChunk>, Inlet<PcmChunk>) {
    connect::<PcmChunk>(pcm_buffer_chunks.max(1) + 2, None)
}

#[derive(serde::Serialize)]
struct ConsumerHangCtx {
    phase: String,
    variant: Option<usize>,
    abr_escaping: Option<bool>,
    abr_locked: Option<bool>,
    abr_pending: Option<String>,
    epoch: u64,
    preloaded: bool,
    block_on_underrun: bool,
}

fn wake_worker(worker: Option<&AudioWorkerHandle>) {
    if let Some(worker) = worker {
        worker.wake();
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
    use crate::audio::ReadOutcome;

    struct RingFixture {
        ring: RingConsumer,
        cursor: ChunkCursor,
        events: crate::audio::event::AudioEvents,
        data_tx: Outlet<Fetch<PcmChunk>>,
        playhead: Arc<PlayheadState>,
        _trash_rx: Inlet<PcmChunk>,
    }

    impl RingFixture {
        fn new(preloaded: bool) -> Self {
            let (data_tx, pcm_rx) = connect::<Fetch<PcmChunk>>(4, None);
            let (trash_tx, trash_rx) = connect::<PcmChunk>(8, None);
            let pool = PcmPool::default().clone();
            let mut ring = RingConsumer::new(RingParts {
                pcm_rx,
                trash_tx,
                reader_wake: Arc::new(ThreadWake::default()),
                epoch: Arc::new(AtomicU64::new(0)),
                block_on_underrun: false,
            });
            ring.preloaded = preloaded;
            Self {
                ring,
                cursor: ChunkCursor::new(&pool, PcmMeta::default().spec),
                events: crate::audio::event::AudioEvents::test(),
                data_tx,
                playhead: Arc::new(PlayheadState::new()),
                _trash_rx: trash_rx,
            }
        }

        fn recv(&mut self) -> Option<PcmChunk> {
            self.ring.recv_valid_chunk(empty_ctx())
        }
    }

    fn empty_ctx() -> RecvCtx<'static> {
        RecvCtx {
            cancel: None,
            worker: None,
            abr: None,
        }
    }

    fn make_chunk(samples: &[f32]) -> PcmChunk {
        let mut meta = PcmMeta::default();
        meta.spec.channels = 1;
        meta.frames = u32::try_from(samples.len()).unwrap_or(u32::MAX);
        PcmChunk::new(meta, PcmPool::default().attach(samples.to_vec()))
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
    #[should_panic(expected = "recv_outcome_blocking")]
    fn blocking_recv_without_preload_panics_when_no_chunk_arrives() {
        let mut fixture = RingFixture::new(false);
        let _ = fixture.recv();
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
            .try_push(Fetch::data(make_chunk(&[0.1, 0.2]), 0))
            .expect("chunk reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));
        assert_eq!(fixture.ring.phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn consumer_phase_transitions_to_seek_pending() {
        let mut fixture = RingFixture::new(true);
        fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);
        assert!(matches!(
            fixture.ring.phase,
            ConsumerPhase::SeekPending { .. }
        ));
    }

    #[kithara::test]
    fn consumer_phase_seek_pending_to_playing_on_chunk() {
        let mut fixture = RingFixture::new(true);
        fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);
        fixture
            .data_tx
            .try_push(Fetch::data(make_chunk(&[0.1, 0.2]), 1))
            .expect("post-seek chunk reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));
        assert_eq!(fixture.ring.phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn seek_recycles_current_chunk_and_varispeed_lookahead() {
        let current = make_chunk(&[0.1, 0.2]);
        let lookahead = make_chunk(&[0.3, 0.4]);
        let mut fixture = RingFixture::new(true);
        fixture.cursor.begin_chunk(&current);
        fixture.ring.current_chunk = Some(current);
        fixture.ring.lookahead = Some(lookahead);

        fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);

        assert!(fixture.ring.current_chunk.is_none());
        assert!(fixture.ring.lookahead.is_none());
        assert!(fixture._trash_rx.try_pop().is_some());
        assert!(fixture._trash_rx.try_pop().is_some());
    }

    #[kithara::test]
    fn seek_drain_preserves_new_epoch_chunk_after_stale_chunks() {
        let mut fixture = RingFixture::new(true);
        fixture
            .data_tx
            .try_push(Fetch::data(make_chunk(&[0.1, 0.2]), 0))
            .expect("stale chunk reaches ring");
        fixture
            .data_tx
            .try_push(Fetch::data(make_chunk(&[0.7, 0.8]), 1))
            .expect("fresh chunk reaches ring");
        fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);
        let mut buf = [0.0; 2];
        let read = fixture
            .cursor
            .read(
                &mut fixture.ring,
                &mut fixture.events,
                fixture.playhead.as_ref(),
                empty_ctx(),
                1.0,
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
            .try_push(Fetch::data(make_chunk(&[0.1, 0.2]), 0))
            .expect("stale chunk reaches ring");
        fixture
            .data_tx
            .try_push(Fetch::eof(1))
            .expect("eof reaches ring");
        fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);
        let mut buf = [0.0; 2];
        let read = fixture
            .cursor
            .read(
                &mut fixture.ring,
                &mut fixture.events,
                fixture.playhead.as_ref(),
                empty_ctx(),
                1.0,
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
        assert_eq!(fixture.ring.phase, ConsumerPhase::Failed);
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
        let _ = eof.recv();
        assert_eq!(eof.ring.phase, ConsumerPhase::AtEof);

        let mut failed = RingFixture::new(true);
        failed
            .data_tx
            .try_push(Fetch::failure(0))
            .expect("failure reaches ring");
        let _ = failed.recv();
        assert_ne!(failed.ring.phase, ConsumerPhase::AtEof);
        assert_eq!(failed.ring.phase, ConsumerPhase::Failed);
    }
}
