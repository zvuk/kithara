use std::sync::atomic::AtomicU64;

use kithara_abr::AbrHandle;
use kithara_events::{DeferredBus, Event};
use kithara_platform::{CancelToken, sync::Arc};
use kithara_signal::AudioChunk;
use kithara_stream::WorkerWake;
use kithara_test_utils::kithara;

use super::{
    ConsumerPhase, ConsumerWakeMode, EpochValidator, FailureSource, Fetch, Inlet, Outlet,
    ThreadWake, WakeSignal, connect, cursor::ChunkCursor, event::ReaderOutputWake,
    park::receive_is_nonblocking,
};
use crate::{SourceEnd, SourceSpan};

enum FetchOutcome {
    Continue,
    Return(Option<(AudioChunk, Option<SourceSpan>)>),
}

pub(super) enum RecvOutcome {
    Closed,
    Empty,
    Item(Fetch<AudioChunk>),
}

#[derive(Clone, Copy)]
pub(super) struct RecvCtx<'a> {
    pub(super) abr: Option<&'a AbrHandle>,
    pub(super) cancel: Option<&'a CancelToken>,
    pub(super) worker: Option<&'a dyn WorkerWake>,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct RingConsumer {
    pub(super) phase: ConsumerPhase,
    pub(super) validator: EpochValidator,
    pub(super) current_chunk: Option<AudioChunk>,
    pub(super) current_source_span: Option<SourceSpan>,
    pub(super) preloaded: bool,
    _epoch: Arc<AtomicU64>,
    reader_wake: Arc<ThreadWake>,
    audio_rx: Inlet<Fetch<AudioChunk>>,
    trash_tx: Outlet<AudioChunk>,
    block_on_underrun: bool,
    #[field(get, vis = "pub(super)", copy)]
    consumer_wake_mode: ConsumerWakeMode,
}

pub(super) struct RingParts {
    pub(super) epoch: Arc<AtomicU64>,
    pub(super) reader_wake: Arc<ThreadWake>,
    pub(super) audio_rx: Inlet<Fetch<AudioChunk>>,
    pub(super) trash_tx: Outlet<AudioChunk>,
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
            audio_rx: parts.audio_rx,
            validator: EpochValidator::default(),
            phase: ConsumerPhase::Buffering,
            current_chunk: None,
            current_source_span: None,
            trash_tx: parts.trash_tx,
            reader_wake: parts.reader_wake,
            _epoch: parts.epoch,
            preloaded: false,
            block_on_underrun: parts.block_on_underrun,
            consumer_wake_mode,
        }
    }

    #[kithara::hang_watchdog]
    #[must_use]
    pub(super) fn begin_seek_epoch(&mut self, epoch: u64, cursor: &mut ChunkCursor) -> bool {
        self.validator.epoch = epoch;
        self.recycle_current();
        cursor.clear();
        self.phase = ConsumerPhase::SeekPending { epoch };

        let mut popped = false;
        while let Some(fetch) = self.audio_rx.try_pop() {
            popped = true;
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
        popped
    }

    pub(super) fn wake_worker(&self, worker: Option<&dyn WorkerWake>) {
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

    pub(super) fn discard(&mut self, chunk: AudioChunk) {
        if let Err(_overflow) = self.trash_tx.try_push(chunk) {
            debug_assert!(
                false,
                "PCM trash ring overflow - spent buffer freed on the audio thread"
            );
        }
    }

    pub(super) fn fill(&mut self, cursor: &mut ChunkCursor, ctx: RecvCtx<'_>) -> bool {
        let Some((chunk, source_span)) = self.recv_valid_chunk(ctx) else {
            return false;
        };
        cursor.begin_chunk(&chunk);
        self.current_chunk = Some(chunk);
        self.current_source_span = source_span;
        self.promote_playing();
        true
    }

    fn process_fetch(&mut self, fetch: Fetch<AudioChunk>) -> FetchOutcome {
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
                self.phase = ConsumerPhase::Failed {
                    source: FailureSource::Producer,
                };
                FetchOutcome::Return(None)
            }
            Fetch::Data {
                data, source_end, ..
            } => {
                let source_span = source_span(&data, source_end);
                FetchOutcome::Return(Some((data, source_span)))
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
                try_pop_and_wake(&mut self.audio_rx, ctx.worker, self.consumer_wake_mode)
            {
                return RecvOutcome::Item(fetch);
            }
            return RecvOutcome::Empty;
        }
        self.recv_outcome_blocking(ctx)
    }

    #[kithara::flash(true)]
    #[kithara::measure(label = "audio.ring.wait")]
    #[kithara::hang_watchdog(ctx = ConsumerHangCtx)]
    fn recv_outcome_blocking(&mut self, ctx: RecvCtx<'_>) -> RecvOutcome {
        loop {
            if let Some(fetch) =
                try_pop_and_wake(&mut self.audio_rx, ctx.worker, self.consumer_wake_mode)
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
                try_pop_and_wake(&mut self.audio_rx, ctx.worker, self.consumer_wake_mode)
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
    pub(super) fn recv_valid_chunk(
        &mut self,
        ctx: RecvCtx<'_>,
    ) -> Option<(AudioChunk, Option<SourceSpan>)> {
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

    pub(super) fn recycle_current(&mut self) {
        self.current_source_span = None;
        if let Some(chunk) = self.current_chunk.take() {
            self.discard(chunk);
        }
    }

    fn stage_post_seek_fetch(
        &mut self,
        fetch: Fetch<AudioChunk>,
        epoch: u64,
        cursor: &mut ChunkCursor,
    ) {
        debug_assert_eq!(
            fetch.epoch(),
            epoch,
            "PCM ring preserved a fetch from a future seek epoch"
        );
        match fetch {
            Fetch::Data {
                data, source_end, ..
            } => {
                self.current_source_span = source_span(&data, source_end);
                cursor.begin_chunk(&data);
                self.current_chunk = Some(data);
                self.phase = ConsumerPhase::Playing;
            }
            Fetch::NaturalEof { .. } => {
                self.current_source_span = None;
                self.phase = ConsumerPhase::AtEof;
            }
            Fetch::Failure { .. } => {
                self.current_source_span = None;
                self.phase = ConsumerPhase::Failed {
                    source: FailureSource::ProducerAfterSeek,
                };
            }
        }
    }
}

fn source_span(data: &AudioChunk, source_end: Option<SourceEnd>) -> Option<SourceSpan> {
    let source_end = source_end?;
    if source_end.sample_rate() != data.meta.spec.sample_rate {
        return None;
    }
    SourceSpan::new(
        data.meta.frame_offset,
        source_end.frame(),
        source_end.sample_rate(),
    )
    .map(|span| span.with_render_revision(data.meta.render_revision))
}

pub(super) fn create_channels(
    audio_buffer_chunks: usize,
    emit: Arc<DeferredBus<Event>>,
    reader_wake: &Arc<ThreadWake>,
) -> (Outlet<Fetch<AudioChunk>>, Inlet<Fetch<AudioChunk>>) {
    let wake: Arc<dyn WakeSignal> = Arc::new(ReaderOutputWake::new(Arc::clone(reader_wake), emit));
    connect::<Fetch<AudioChunk>>(audio_buffer_chunks.max(1), Some(wake))
}

pub(super) fn create_trash_channel(
    audio_buffer_chunks: usize,
) -> (Outlet<AudioChunk>, Inlet<AudioChunk>) {
    connect::<AudioChunk>(audio_buffer_chunks.max(1) + 2, None)
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
    audio_rx: &mut Inlet<Fetch<AudioChunk>>,
    worker: Option<&dyn WorkerWake>,
    mode: ConsumerWakeMode,
) -> Option<Fetch<AudioChunk>> {
    let fetch = audio_rx.try_pop()?;
    wake_worker(worker, mode);
    Some(fetch)
}

fn wake_worker(worker: Option<&dyn WorkerWake>, mode: ConsumerWakeMode) {
    let Some(worker) = worker else {
        return;
    };
    match mode {
        ConsumerWakeMode::RealtimeDeferred => worker.defer(),
        ConsumerWakeMode::ImmediateOffRt => worker.wake(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use kithara_platform::{CancelToken, sync::Arc};
    use kithara_signal::{AudioChunk, AudioChunkInfo};
    use kithara_stream::PlayheadState;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        ConsumerWakeMode,
        audio::ReadOutcome,
        test_pools::{Pools, pools, sample_buffer},
    };

    struct RingFixture {
        pools: Pools,
        playhead: Arc<PlayheadState>,
        events: crate::audio::event::AudioEvents,
        cursor: ChunkCursor,
        _trash_rx: Inlet<AudioChunk>,
        data_tx: Outlet<Fetch<AudioChunk>>,
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
            let pools = pools();
            let (data_tx, audio_rx) = connect::<Fetch<AudioChunk>>(4, None);
            let (trash_tx, trash_rx) = connect::<AudioChunk>(8, None);
            let mut ring = RingConsumer::new(RingParts {
                audio_rx,
                trash_tx,
                reader_wake: Arc::new(ThreadWake::default()),
                epoch: Arc::new(AtomicU64::new(0)),
                block_on_underrun,
                consumer_wake_mode,
            });
            ring.preloaded = preloaded;
            Self {
                cursor: ChunkCursor::new(&pools, AudioChunkInfo::default().spec)
                    .expect("cursor scratch fits test pools"),
                pools,
                ring,
                data_tx,
                events: crate::audio::event::AudioEvents::test(),
                playhead: Arc::new(PlayheadState::new()),
                _trash_rx: trash_rx,
            }
        }

        fn recv(&mut self) -> Option<AudioChunk> {
            self.ring
                .recv_valid_chunk(empty_ctx())
                .map(|(chunk, _source_span)| chunk)
        }

        fn chunk(&self, samples: &[f32]) -> AudioChunk {
            let mut meta = AudioChunkInfo::default();
            meta.spec.channels = 1;
            meta.frames = u32::try_from(samples.len()).unwrap_or(u32::MAX);
            AudioChunk::new(meta, sample_buffer(&self.pools, samples))
        }
    }

    fn empty_ctx() -> RecvCtx<'static> {
        RecvCtx {
            cancel: None,
            worker: None,
            abr: None,
        }
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
        let fixture = RingFixture::with_wake_mode(true, false, ConsumerWakeMode::ImmediateOffRt);

        assert_eq!(
            fixture.ring.consumer_wake_mode,
            ConsumerWakeMode::ImmediateOffRt
        );
    }

    #[kithara::test]
    fn seek_drain_reports_whether_it_popped_any_item() {
        let mut drained = RingFixture::new(true);
        let first = drained.chunk(&[0.1]);
        drained
            .data_tx
            .try_push(Fetch::data(first, 0))
            .expect("first stale chunk reaches ring");
        let second = drained.chunk(&[0.2]);
        drained
            .data_tx
            .try_push(Fetch::data(second, 0))
            .expect("second stale chunk reaches ring");
        drained
            .data_tx
            .try_push(Fetch::eof(1))
            .expect("current epoch marker reaches ring");

        assert!(drained.ring.begin_seek_epoch(1, &mut drained.cursor));

        let mut empty = RingFixture::new(true);
        assert!(!empty.ring.begin_seek_epoch(1, &mut empty.cursor));
    }

    /// One second instead of the ambient ten: the watchdog park is the point of
    /// this test, and on the flash-off lane that park is spent in real time
    /// (measured: 10.087 s ambient vs 0.074 s under flash, where it is virtual).
    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(hang_timeout_secs(1))]
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
        let chunk = fixture.chunk(&[0.1, 0.2]);
        fixture
            .data_tx
            .try_push(Fetch::data(chunk, 0))
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
        let chunk = fixture.chunk(&[0.1, 0.2]);
        fixture
            .data_tx
            .try_push(Fetch::data(chunk, 1))
            .expect("post-seek chunk reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));
        assert_eq!(fixture.ring.phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn seek_drain_preserves_new_epoch_chunk_after_stale_chunks() {
        let mut fixture = RingFixture::new(true);
        let stale = fixture.chunk(&[0.1, 0.2]);
        fixture
            .data_tx
            .try_push(Fetch::data(stale, 0))
            .expect("stale chunk reaches ring");
        let fresh = fixture.chunk(&[0.7, 0.8]);
        fixture
            .data_tx
            .try_push(Fetch::data(fresh, 1))
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
        let stale = fixture.chunk(&[0.1, 0.2]);
        fixture
            .data_tx
            .try_push(Fetch::data(stale, 0))
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
