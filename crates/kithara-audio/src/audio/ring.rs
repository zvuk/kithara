use std::{num::NonZeroUsize, sync::atomic::AtomicU64};

use kithara_abr::AbrHandle;
use kithara_events::DeferredBus;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_signal::AudioChunk;
use kithara_stream::WorkerWake;
use kithara_test_utils::kithara;

use super::{
    AudioLaneEvent, ConsumerPhase, ConsumerWakeMode, EpochValidator, FailureSource, Fetch, Inlet,
    Outlet, ThreadWake, WakeSignal, connect, cursor::ChunkCursor, event::ReaderOutputWake,
    park::receive_is_nonblocking,
};
use crate::{RevisionFloorStatus, SourceEnd, SourceSpan};

enum FetchOutcome {
    Continue,
    Future(Fetch<AudioChunk>),
    Return(Option<(AudioChunk, Option<SourceSpan>)>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SeekEpochStatus {
    WaitingForPcm,
    Ready,
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
    future_fetch: Option<Fetch<AudioChunk>>,
    pub(super) preloaded: bool,
    _epoch: Arc<AtomicU64>,
    reader_wake: Arc<ThreadWake>,
    #[field(get, vis = "pub(super)", copy)]
    consumer_wake_mode: ConsumerWakeMode,
    audio_rx: Inlet<Fetch<AudioChunk>>,
    rendered_source_head: Option<SourceEnd>,
    rendered_warp_revision: Option<u64>,
    render_revision_floor: u64,
    trash_tx: Outlet<AudioChunk>,
    block_on_underrun: bool,
}

pub(super) struct RingParts {
    pub(super) epoch: Arc<AtomicU64>,
    pub(super) reader_wake: Arc<ThreadWake>,
    pub(super) consumer_wake_mode: ConsumerWakeMode,
    pub(super) audio_rx: Inlet<Fetch<AudioChunk>>,
    pub(super) trash_tx: Outlet<AudioChunk>,
    pub(super) block_on_underrun: bool,
}

pub(super) trait SeekEpochReadiness {
    fn seek_epoch_status(&mut self, epoch: u64) -> SeekEpochStatus;
}

impl SeekEpochReadiness for RingConsumer {
    fn seek_epoch_status(&mut self, epoch: u64) -> SeekEpochStatus {
        let parked = self
            .future_fetch
            .as_ref()
            .is_some_and(|fetch| fetch.epoch() == epoch);
        let queued = self
            .audio_rx
            .fold(false, |ready, fetch| ready || fetch.epoch() == epoch);
        if parked || queued {
            SeekEpochStatus::Ready
        } else {
            SeekEpochStatus::WaitingForPcm
        }
    }
}

impl RingConsumer {
    pub(super) fn new(parts: RingParts) -> Self {
        let consumer_wake_mode =
            resolve_wake_mode(parts.consumer_wake_mode, parts.block_on_underrun);
        Self {
            consumer_wake_mode,
            audio_rx: parts.audio_rx,
            validator: EpochValidator::default(),
            phase: ConsumerPhase::Buffering,
            current_chunk: None,
            current_source_span: None,
            future_fetch: None,
            rendered_source_head: None,
            rendered_warp_revision: None,
            render_revision_floor: 0,
            trash_tx: parts.trash_tx,
            reader_wake: parts.reader_wake,
            _epoch: parts.epoch,
            preloaded: false,
            block_on_underrun: parts.block_on_underrun,
        }
    }

    #[kithara::hang_watchdog]
    #[must_use]
    pub(super) fn begin_seek_epoch(&mut self, epoch: u64, cursor: &mut ChunkCursor) -> bool {
        self.validator.epoch = epoch;
        self.recycle_current();
        self.rendered_source_head = None;
        self.rendered_warp_revision = None;
        cursor.clear();
        self.phase = ConsumerPhase::SeekPending { epoch };

        let mut popped = self.future_fetch.is_some();
        if let Some(fetch) = self.future_fetch.take() {
            if fetch.epoch() == epoch || is_producer_terminal(&fetch) {
                self.stage_post_seek_fetch(fetch, epoch, cursor);
                return true;
            }
            if let Fetch::Data { data, .. } = fetch {
                self.discard(data);
            }
        }
        while let Some(fetch) = self.audio_rx.try_pop() {
            popped = true;
            if fetch.epoch() < epoch && !is_producer_terminal(&fetch) {
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

    pub(super) fn set_render_revision_floor(
        &mut self,
        revision: u64,
        required_frames: NonZeroUsize,
        presented_source: Option<SourceEnd>,
        replacement_epoch: Option<u64>,
        cursor: &mut ChunkCursor,
        ctx: RecvCtx<'_>,
    ) -> RevisionFloorStatus {
        apply_render_revision_floor(
            self,
            revision,
            required_frames,
            presented_source,
            replacement_epoch,
            cursor,
            ctx,
        )
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
        if fetch.epoch() > self.validator.epoch && !is_producer_terminal(&fetch) {
            return FetchOutcome::Future(fetch);
        }
        if !self.validator.is_valid(&fetch) && !is_producer_terminal(&fetch) {
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
                data,
                epoch,
                source_end,
            } => {
                if data.meta.render_revision < self.render_revision_floor {
                    kithara::probe_event!(
                        pcm_revision_discarded,
                        revision = data.meta.render_revision
                    );
                    self.discard(data);
                    return FetchOutcome::Continue;
                }
                let source_span = self.source_span(&data, source_end);
                if let Some(source) = source_span {
                    kithara::probe_event!(
                        pcm_reader_admitted,
                        seek_epoch = epoch,
                        source_start = source.start(),
                        source_end = source.end(),
                        render_revision = source.render_revision()
                    );
                }
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
        if self.future_fetch.is_some() {
            return RecvOutcome::Empty;
        }
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
                    FetchOutcome::Future(fetch) => {
                        self.future_fetch = Some(fetch);
                        return None;
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

    pub(super) fn set_consumer_wake_mode(&mut self, mode: ConsumerWakeMode) {
        self.consumer_wake_mode = resolve_wake_mode(mode, self.block_on_underrun);
    }

    fn source_span(
        &mut self,
        data: &AudioChunk,
        source_end: Option<SourceEnd>,
    ) -> Option<SourceSpan> {
        let Some(source_end) = source_end else {
            self.rendered_source_head = None;
            self.rendered_warp_revision = None;
            return None;
        };
        if source_end.sample_rate() != data.meta.spec.sample_rate {
            self.rendered_source_head = None;
            self.rendered_warp_revision = None;
            return None;
        }
        let source_start = self
            .rendered_source_head
            .filter(|head| {
                head.sample_rate() == source_end.sample_rate()
                    && self.rendered_warp_revision
                        == Some(kithara_signal::render_warp_map_revision(
                            data.meta.render_revision,
                        ))
            })
            .map_or(data.meta.frame_offset, |head| head.frame());
        self.rendered_source_head = Some(source_end);
        self.rendered_warp_revision = Some(kithara_signal::render_warp_map_revision(
            data.meta.render_revision,
        ));
        SourceSpan::new(source_start, source_end.frame(), source_end.sample_rate())
            .map(|span| span.with_render_revision(data.meta.render_revision))
    }

    fn stage_post_seek_fetch(
        &mut self,
        fetch: Fetch<AudioChunk>,
        epoch: u64,
        cursor: &mut ChunkCursor,
    ) {
        debug_assert!(
            fetch.epoch() == epoch || is_producer_terminal(&fetch),
            "PCM ring preserved an epoch-scoped fetch from another seek epoch"
        );
        match fetch {
            Fetch::Data {
                data, source_end, ..
            } => {
                self.current_source_span = self.source_span(&data, source_end);
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

    pub(super) fn wake_worker(&self, worker: Option<&dyn WorkerWake>) {
        wake_worker(worker, self.consumer_wake_mode);
    }
}

fn apply_render_revision_floor(
    consumer: &mut RingConsumer,
    revision: u64,
    required_frames: NonZeroUsize,
    presented_source: Option<SourceEnd>,
    replacement_epoch: Option<u64>,
    cursor: &mut ChunkCursor,
    ctx: RecvCtx<'_>,
) -> RevisionFloorStatus {
    let stale = consumer
        .current_chunk
        .as_ref()
        .is_some_and(|chunk| chunk.meta.render_revision < revision);
    let current_frames = consumer.current_chunk.as_ref().map_or(0, |chunk| {
        if replacement_epoch.is_none() && chunk.meta.render_revision >= revision {
            cursor.remaining_frames(chunk)
        } else {
            0
        }
    });
    let future_frames = consumer.future_fetch.as_ref().map_or(0, |fetch| {
        let eligible = replacement_epoch.is_some_and(|epoch| fetch.epoch() == epoch)
            && matches!(fetch, Fetch::Data { data, .. } if data.meta.render_revision >= revision);
        if eligible {
            let Fetch::Data { data, .. } = fetch else {
                unreachable!();
            };
            usize::try_from(data.meta.frames).unwrap_or(usize::MAX)
        } else {
            0
        }
    });
    let prepared_frames = consumer.audio_rx.fold(
        current_frames.saturating_add(future_frames),
        |frames, fetch| {
            let eligible = replacement_epoch.map_or_else(
                || consumer.validator.is_valid(fetch),
                |epoch| fetch.epoch() == epoch,
            ) && matches!(fetch, Fetch::Data { data, .. } if data.meta.render_revision >= revision);
            if eligible {
                let Fetch::Data { data, .. } = fetch else {
                    unreachable!();
                };
                frames.saturating_add(usize::try_from(data.meta.frames).unwrap_or(usize::MAX))
            } else {
                frames
            }
        },
    );
    if prepared_frames < required_frames.get() {
        return RevisionFloorStatus::WaitingForReplacement;
    }
    if replacement_epoch.is_some_and(|epoch| epoch != consumer.validator.epoch) {
        return RevisionFloorStatus::ReadyForSeekPresentation;
    }
    if consumer.current_chunk.is_some() && !stale {
        return RevisionFloorStatus::Current;
    }
    if revision > consumer.render_revision_floor {
        consumer.render_revision_floor = revision;
        consumer.rendered_source_head = presented_source;
        consumer.rendered_warp_revision =
            presented_source.map(|_| kithara_signal::render_warp_map_revision(revision));
    }
    let Some((replacement, source_span)) = consumer.recv_valid_chunk(ctx) else {
        return RevisionFloorStatus::WaitingForReplacement;
    };
    if stale {
        kithara::probe_event!(
            pcm_revision_discarded,
            revision = consumer
                .current_chunk
                .as_ref()
                .map_or(0, |chunk| chunk.meta.render_revision)
        );
        consumer.recycle_current();
    }
    cursor.begin_chunk(&replacement);
    consumer.current_chunk = Some(replacement);
    consumer.current_source_span = source_span;
    consumer.promote_playing();
    RevisionFloorStatus::Switched
}

/// A consumer that blocks on underrun waits on the producer thread, so it wakes
/// it inline whatever the session declares.
const fn resolve_wake_mode(mode: ConsumerWakeMode, block_on_underrun: bool) -> ConsumerWakeMode {
    if block_on_underrun {
        ConsumerWakeMode::ImmediateOffRt
    } else {
        mode
    }
}

pub(super) fn create_channels(
    audio_buffer_chunks: usize,
    emit: Arc<DeferredBus<AudioLaneEvent>>,
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

/// Whether `fetch` reports a producer that will never produce again.
///
/// A failure marker is pushed once, immediately before the produce task
/// retires, and the track FSM never leaves `Failed` — so no later epoch
/// can restate it and the marker must terminalise the consumer whatever
/// epoch it carries. Epoch scoping stays on data and natural EOF, both of
/// which a live producer re-derives after a seek.
const fn is_producer_terminal(fetch: &Fetch<AudioChunk>) -> bool {
    matches!(fetch, Fetch::Failure { .. })
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
    use kithara_test_fixtures::mock_fixtures::ring_pcm;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        ConsumerWakeMode,
        audio::ReadOutcome,
        test_pools::{Pools, pools, sample_buffer},
    };

    struct RingFixture {
        playhead: Arc<PlayheadState>,
        events: crate::audio::event::AudioEvents,
        cursor: ChunkCursor,
        trash_rx: Inlet<AudioChunk>,
        data_tx: Outlet<Fetch<AudioChunk>>,
        pools: Pools,
        ring: RingConsumer,
    }

    impl RingFixture {
        fn new(preloaded: bool) -> Self {
            Self::with_wake_mode(preloaded, false, ConsumerWakeMode::RealtimeDeferred)
        }

        fn chunk(&self, samples: &[f32]) -> AudioChunk {
            let mut meta = AudioChunkInfo::default();
            meta.spec.channels = 1;
            meta.frames = u32::try_from(samples.len()).unwrap_or(u32::MAX);
            AudioChunk::new(meta, sample_buffer(&self.pools, samples))
        }

        fn recv(&mut self) -> Option<AudioChunk> {
            self.ring
                .recv_valid_chunk(empty_ctx())
                .map(|(chunk, _source_span)| chunk)
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
                block_on_underrun,
                consumer_wake_mode,
                reader_wake: Arc::new(ThreadWake::default()),
                epoch: Arc::new(AtomicU64::new(0)),
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
                trash_rx,
            }
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
    fn an_adopted_mode_still_yields_to_blocking_reads() {
        let mut fixture =
            RingFixture::with_wake_mode(false, true, ConsumerWakeMode::ImmediateOffRt);

        fixture
            .ring
            .set_consumer_wake_mode(ConsumerWakeMode::RealtimeDeferred);

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
    fn rendered_revision_change_starts_a_new_source_span() {
        let mut fixture = RingFixture::new(true);
        let mut old = fixture.chunk(&[1.0; 4]);
        old.meta.frame_offset = 100;
        old.meta.render_revision =
            kithara_signal::pack_render_revision(1, 7).expect("fixture revision fits");
        let old_span = fixture
            .ring
            .source_span(&old, Some(SourceEnd::new(104, old.meta.spec.sample_rate)))
            .expect("old source span");
        assert_eq!((old_span.start(), old_span.end()), (100, 104));

        let mut new = fixture.chunk(&[2.0; 4]);
        new.meta.frame_offset = 40;
        new.meta.render_revision =
            kithara_signal::pack_render_revision(1, 8).expect("fixture revision fits");
        let new_span = fixture
            .ring
            .source_span(&new, Some(SourceEnd::new(44, new.meta.spec.sample_rate)))
            .expect("new source span");
        assert_eq!((new_span.start(), new_span.end()), (40, 44));
    }

    #[kithara::test]
    fn seek_drain_reports_whether_it_popped_any_item(ring_pcm: Vec<f32>) {
        let mut drained = RingFixture::new(true);
        let first = drained.chunk(&ring_pcm[..1]);
        drained
            .data_tx
            .try_push(Fetch::data(first, 0))
            .expect("first stale chunk reaches ring");
        let second = drained.chunk(&ring_pcm[1..2]);
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

    #[kithara::test]
    fn seek_epoch_becomes_ready_only_when_its_fetch_is_queued(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        assert_eq!(
            fixture.ring.seek_epoch_status(1),
            SeekEpochStatus::WaitingForPcm
        );

        let old = fixture.chunk(&ring_pcm[..1]);
        fixture
            .data_tx
            .try_push(Fetch::data(old, 0))
            .expect("old epoch reaches ring");
        assert_eq!(
            fixture.ring.seek_epoch_status(1),
            SeekEpochStatus::WaitingForPcm
        );

        let replacement = fixture.chunk(&ring_pcm[1..2]);
        fixture
            .data_tx
            .try_push(Fetch::data(replacement, 1))
            .expect("replacement epoch reaches ring");
        assert_eq!(fixture.ring.seek_epoch_status(1), SeekEpochStatus::Ready);
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
    fn consumer_phase_transitions_to_playing_on_first_chunk(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        let chunk = fixture.chunk(&ring_pcm[..2]);
        fixture
            .data_tx
            .try_push(Fetch::data(chunk, 0))
            .expect("chunk reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));
        assert_eq!(fixture.ring.phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn render_revision_floor_discards_current_and_queued_stale_pcm(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        let mut current = fixture.chunk(&ring_pcm[..1]);
        current.meta.render_revision = 7;
        fixture
            .data_tx
            .try_push(Fetch::data(current, 0))
            .expect("current stale chunk reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));

        for revision in [8, 10, 11] {
            let mut chunk = fixture.chunk(&ring_pcm[..1]);
            chunk.meta.render_revision = revision;
            fixture
                .data_tx
                .try_push(Fetch::data(chunk, 0))
                .expect("revisioned chunk reaches ring");
        }

        assert_eq!(
            fixture.ring.set_render_revision_floor(
                10,
                NonZeroUsize::new(1).expect("fixture interval is non-zero"),
                None,
                None,
                &mut fixture.cursor,
                empty_ctx(),
            ),
            RevisionFloorStatus::Switched
        );
        assert_eq!(
            fixture
                .ring
                .current_chunk
                .as_ref()
                .map(|chunk| chunk.meta.render_revision),
            Some(10)
        );
        assert_eq!(
            fixture
                .trash_rx
                .try_pop()
                .map(|chunk| chunk.meta.render_revision),
            Some(8)
        );
        assert_eq!(
            fixture
                .trash_rx
                .try_pop()
                .map(|chunk| chunk.meta.render_revision),
            Some(7)
        );
        assert_eq!(
            fixture.recv().map(|chunk| chunk.meta.render_revision),
            Some(11)
        );
    }

    #[kithara::test]
    fn render_revision_floor_keeps_current_pcm_until_replacement_is_ready(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        let mut current = fixture.chunk(&ring_pcm[..1]);
        current.meta.render_revision = 7;
        fixture
            .data_tx
            .try_push(Fetch::data(current, 0))
            .expect("current chunk reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));

        assert_eq!(
            fixture.ring.set_render_revision_floor(
                10,
                NonZeroUsize::new(1).expect("fixture interval is non-zero"),
                None,
                None,
                &mut fixture.cursor,
                empty_ctx(),
            ),
            RevisionFloorStatus::WaitingForReplacement
        );
        assert_eq!(
            fixture
                .ring
                .current_chunk
                .as_ref()
                .map(|chunk| chunk.meta.render_revision),
            Some(7)
        );
        assert!(fixture.trash_rx.try_pop().is_none());
    }

    #[kithara::test]
    fn revision_switch_waits_for_the_complete_requested_interval(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        let mut current = fixture.chunk(&ring_pcm[..1]);
        current.meta.render_revision = 7;
        fixture
            .data_tx
            .try_push(Fetch::data(current, 0))
            .expect("current PCM reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));

        let mut first = fixture.chunk(&ring_pcm[1..2]);
        first.meta.render_revision = 10;
        fixture
            .data_tx
            .try_push(Fetch::data(first, 0))
            .expect("partial replacement reaches ring");
        let required = NonZeroUsize::new(2).expect("fixture interval is non-zero");
        assert_eq!(
            fixture.ring.set_render_revision_floor(
                10,
                required,
                None,
                None,
                &mut fixture.cursor,
                empty_ctx(),
            ),
            RevisionFloorStatus::WaitingForReplacement
        );
        assert_eq!(
            fixture
                .ring
                .current_chunk
                .as_ref()
                .map(|chunk| chunk.meta.render_revision),
            Some(7)
        );

        let mut second = fixture.chunk(&ring_pcm[2..3]);
        second.meta.render_revision = 10;
        fixture
            .data_tx
            .try_push(Fetch::data(second, 0))
            .expect("complete replacement reaches ring");
        assert_eq!(
            fixture.ring.set_render_revision_floor(
                10,
                required,
                None,
                None,
                &mut fixture.cursor,
                empty_ctx(),
            ),
            RevisionFloorStatus::Switched
        );
    }

    #[kithara::test]
    fn unavailable_revision_preserves_pcm_across_current_chunk_boundary(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        for sample in &ring_pcm[..2] {
            let mut chunk = fixture.chunk(&[*sample]);
            chunk.meta.render_revision = 7;
            fixture
                .data_tx
                .try_push(Fetch::data(chunk, 0))
                .expect("old PCM reaches ring");
        }
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));
        assert_eq!(
            fixture.ring.set_render_revision_floor(
                10,
                NonZeroUsize::new(2).expect("fixture interval is non-zero"),
                None,
                None,
                &mut fixture.cursor,
                empty_ctx(),
            ),
            RevisionFloorStatus::WaitingForReplacement
        );

        let mut output = [0.0; 2];
        let read = fixture
            .cursor
            .read(
                &mut fixture.ring,
                &mut fixture.events,
                fixture.playhead.as_ref(),
                empty_ctx(),
                &mut output,
            )
            .expect("retained PCM remains readable");
        let ReadOutcome::Frames { count, .. } = read.outcome else {
            panic!("old PCM must remain available until replacement arrives");
        };
        assert_eq!(
            count.get(),
            2,
            "unavailable replacement must not create a hole"
        );
        assert_eq!(output.as_slice(), &ring_pcm[..2]);
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
    fn consumer_phase_seek_pending_to_playing_on_chunk(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        let _ = fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);
        let chunk = fixture.chunk(&ring_pcm[..2]);
        fixture
            .data_tx
            .try_push(Fetch::data(chunk, 1))
            .expect("post-seek chunk reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));
        assert_eq!(fixture.ring.phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn future_seek_pcm_waits_for_explicit_presentation(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        let future = fixture.chunk(&ring_pcm[..2]);
        fixture
            .data_tx
            .try_push(Fetch::data(future, 1))
            .expect("future chunk reaches ring");

        assert!(fixture.recv().is_none());
        assert!(fixture.trash_rx.try_pop().is_none());
        assert_eq!(
            fixture.ring.set_render_revision_floor(
                0,
                NonZeroUsize::new(2).expect("replacement length is non-zero"),
                None,
                Some(1),
                &mut fixture.cursor,
                empty_ctx(),
            ),
            RevisionFloorStatus::ReadyForSeekPresentation
        );
        assert!(fixture.ring.begin_seek_epoch(1, &mut fixture.cursor));

        let mut output = [0.0; 2];
        let read = fixture
            .cursor
            .read(
                &mut fixture.ring,
                &mut fixture.events,
                fixture.playhead.as_ref(),
                empty_ctx(),
                &mut output,
            )
            .expect("presented future PCM remains readable");
        let ReadOutcome::Frames { count, .. } = read.outcome else {
            panic!("presented future PCM must produce frames");
        };
        assert_eq!(count.get(), 2);
        assert_eq!(output.as_slice(), &ring_pcm[..2]);
    }

    #[kithara::test]
    fn future_seek_readiness_counts_only_the_epoch_that_will_be_presented(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        let mut current = fixture.chunk(&ring_pcm[..1]);
        current.meta.render_revision = 10;
        fixture
            .data_tx
            .try_push(Fetch::data(current, 0))
            .expect("current epoch PCM reaches ring");
        assert!(fixture.ring.fill(&mut fixture.cursor, empty_ctx()));

        let mut first = fixture.chunk(&ring_pcm[1..2]);
        first.meta.render_revision = 10;
        fixture
            .data_tx
            .try_push(Fetch::data(first, 1))
            .expect("partial future epoch PCM reaches ring");
        let required = NonZeroUsize::new(2).expect("fixture interval is non-zero");
        assert_eq!(
            fixture.ring.set_render_revision_floor(
                10,
                required,
                None,
                Some(1),
                &mut fixture.cursor,
                empty_ctx(),
            ),
            RevisionFloorStatus::WaitingForReplacement,
            "presenting the future epoch discards current-epoch PCM"
        );

        let mut second = fixture.chunk(&ring_pcm[2..3]);
        second.meta.render_revision = 10;
        fixture
            .data_tx
            .try_push(Fetch::data(second, 1))
            .expect("complete future epoch PCM reaches ring");
        assert_eq!(
            fixture.ring.set_render_revision_floor(
                10,
                required,
                None,
                Some(1),
                &mut fixture.cursor,
                empty_ctx(),
            ),
            RevisionFloorStatus::ReadyForSeekPresentation
        );
    }

    #[kithara::test]
    fn seek_drain_preserves_new_epoch_chunk_after_stale_chunks(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        let stale = fixture.chunk(&ring_pcm[..2]);
        fixture
            .data_tx
            .try_push(Fetch::data(stale, 0))
            .expect("stale chunk reaches ring");
        let fresh = fixture.chunk(&ring_pcm[2..]);
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
    fn seek_drain_preserves_new_epoch_eof_after_stale_chunks(ring_pcm: Vec<f32>) {
        let mut fixture = RingFixture::new(true);
        let stale = fixture.chunk(&ring_pcm[..2]);
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

    #[kithara::test]
    fn a_stale_producer_failure_survives_a_new_seek_epoch() {
        let mut fixture = RingFixture::new(true);
        fixture
            .data_tx
            .try_push(Fetch::failure(0))
            .expect("failure reaches ring");

        let _ = fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);

        assert_eq!(
            fixture.ring.phase,
            ConsumerPhase::Failed {
                source: FailureSource::ProducerAfterSeek
            }
        );
    }

    #[kithara::test]
    fn a_stale_natural_eof_does_not_terminate_a_new_seek_epoch() {
        let mut fixture = RingFixture::new(true);
        fixture
            .data_tx
            .try_push(Fetch::eof(0))
            .expect("natural eof reaches ring");

        let _ = fixture.ring.begin_seek_epoch(1, &mut fixture.cursor);

        assert_eq!(fixture.ring.phase, ConsumerPhase::SeekPending { epoch: 1 });
    }

    #[kithara::test]
    fn a_stale_producer_failure_terminates_the_consumer() {
        let mut fixture = RingFixture::new(true);
        fixture.ring.validator.epoch = 3;
        fixture
            .data_tx
            .try_push(Fetch::failure(0))
            .expect("failure reaches ring");

        let _chunk = fixture.recv();

        assert_eq!(
            fixture.ring.phase,
            ConsumerPhase::Failed {
                source: FailureSource::Producer
            }
        );
    }
}
