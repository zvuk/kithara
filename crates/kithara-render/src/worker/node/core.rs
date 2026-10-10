use std::task::{Context, Poll};

use kithara_audio::{AudioSource, Fetch, TrackFailureKind, TrackStep, WaitingReason};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::{CancelToken, sync::Arc, time::WallInstant};
use kithara_signal::{AudioChunk, AudioChunkInfo, FrameCount, SegmentId};
use kithara_stream::ActivityWriter;
use kithara_test_utils::kithara;
use kithara_worker::{Priority, Task, TickResult};
use ringbuf::traits::{Consumer, Observer, Producer};

use super::{
    super::{EngineLoad, PcmPacket, reader::PcmProducer},
    pending::PendingPacket,
};
use crate::{LoadRefusal, ServiceClass, WarpSource, dispatcher::LaneTask};

/// Worker-owned source, rendering, ring producer and transport publisher.
pub struct DecoderNode<T, S> {
    pub(super) source: WarpSource<T, S>,
    port: PcmProducer,
    activity: Option<ActivityWriter>,
    priority: ServiceClass,
    pub(super) pending: Option<PendingPacket>,
    pub(super) terminal: Option<Result<SegmentId, TrackFailureKind>>,
    load_error: Option<TrackFailureKind>,
    last_output: AudioChunkInfo,
    pub(super) engine_load: Option<Arc<EngineLoad>>,
    pools: PoolRegion<S>,
    cancel: Option<CancelToken>,
}

impl<T, S> DecoderNode<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub(in crate::worker) fn new(
        source: WarpSource<T, S>,
        port: PcmProducer,
        activity: Option<ActivityWriter>,
        initial: AudioChunkInfo,
        engine_load: Option<Arc<EngineLoad>>,
        pools: PoolRegion<S>,
        cancel: Option<CancelToken>,
    ) -> Self {
        Self {
            source,
            port,
            activity,
            priority: ServiceClass::Warm,
            pending: None,
            terminal: None,
            load_error: None,
            last_output: initial,
            engine_load,
            pools,
            cancel,
        }
    }

    pub(in crate::worker) fn engine_latency(&self) -> FrameCount {
        self.source.engine_latency()
    }

    pub(super) fn synchronize(&mut self) -> Result<(), TrackFailureKind> {
        self.source.service_commands()?;
        let segment = self.source.cursor().segment;
        if self
            .terminal
            .is_some_and(|terminal| matches!(terminal, Ok(ended) if ended != segment))
        {
            self.terminal = None;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| match &pending.packet {
                PcmPacket::Chunk(chunk) => chunk.meta.segment != segment,
                PcmPacket::Failed { .. } => false,
            })
            && let Some(pending) = self.pending.take()
        {
            self.retire(pending.packet);
        }
        Ok(())
    }

    fn retire(&self, packet: PcmPacket) {
        if let PcmPacket::Chunk(chunk) = packet {
            self.source.retire_chunk(*chunk);
        }
    }

    fn fail(&mut self, failure: TrackFailureKind) {
        if matches!(self.terminal, Some(Err(_)))
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| matches!(pending.packet, PcmPacket::Failed { .. }))
        {
            return;
        }
        self.load_error = Some(failure);
        self.pending = Some(PendingPacket {
            packet: PcmPacket::Failed {
                segment: self.source.cursor().segment,
                failure,
            },
            source_end: None,
        });
    }

    fn admit(&mut self) -> TickResult {
        let Some(pending) = self.pending.take() else {
            return TickResult::Waiting;
        };
        let (meta, terminal) = match &pending.packet {
            PcmPacket::Chunk(chunk) => (
                Some(chunk.meta),
                chunk.meta.end_of_track.then_some(Ok(chunk.meta.segment)),
            ),
            PcmPacket::Failed { failure, .. } => (None, Some(Err(*failure))),
        };
        match self.port.forward.try_push(pending.packet) {
            Ok(()) => {
                if let Some(meta) = meta {
                    self.last_output = meta;
                    if meta.segment == self.source.cursor().segment {
                        if let Some(end) = pending.source_end {
                            self.source.commit_source_end(end, meta);
                        }
                        if !meta.end_of_track && meta.frames > 0 {
                            self.source.admitted();
                        }
                        kithara::probe_event!(chunk_admitted, segment = meta.segment.get());
                    }
                }
                if let Some(terminal) = terminal {
                    self.source.finish_preload();
                    if terminal.is_ok() {
                        self.source.finish_segment();
                    }
                    self.terminal = Some(terminal);
                }
                self.port.signal();
                TickResult::Progress
            }
            Err(packet) => {
                self.pending = Some(PendingPacket {
                    packet,
                    source_end: pending.source_end,
                });
                TickResult::Backpressured
            }
        }
    }

    fn eof(&mut self) {
        let cursor = self.source.cursor();
        let source_span = (self.last_output.segment == cursor.segment)
            .then_some(self.last_output.source_span)
            .flatten()
            .and_then(|span| span.for_output_range(span.output_frames()..span.output_frames()));
        let position = source_span
            .and_then(|span| span.position_at(0))
            .unwrap_or_default();
        let mut samples = self.pools.get::<f32>();
        samples.clear();
        let meta = AudioChunkInfo {
            segment: cursor.segment,
            lane_frame: cursor.frame,
            timestamp: position,
            end_timestamp: position,
            frames: 0,
            end_of_track: true,
            source_span,
            ..self.last_output
        };
        self.pending = Some(PendingPacket {
            packet: PcmPacket::Chunk(Box::new(AudioChunk::new(meta, samples))),
            source_end: None,
        });
    }
}

impl<T, S> LaneTask for DecoderNode<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn set_priority(&mut self, class: ServiceClass) {
        self.priority = class;
    }

    fn poll_commands(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.source.poll_commands(cx)
    }

    fn preload_status(&mut self) -> Result<bool, LoadRefusal> {
        if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            return Err(LoadRefusal::Cancelled);
        }
        if let Some(error) = self.load_error.take() {
            return Err(LoadRefusal::Source(error));
        }
        Ok(self.source.is_preloaded())
    }
}

impl<T, S> Task for DecoderNode<T, S>
where
    T: AudioSource<Chunk = AudioChunk>,
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn priority(&self) -> Option<Priority> {
        Some(self.priority.into())
    }

    fn on_cancel(&mut self) {
        if let Some(activity) = &mut self.activity {
            activity.set_playing(false);
        }
    }

    fn recycle(&mut self) {
        while let Some(packet) = self.port.reverse.try_pop() {
            self.retire(packet);
        }
        if let Some(activity) = &mut self.activity {
            activity.set_playing(*self.port.playing.read());
        }
        let _ = self.source.prepare_deferred();
        self.source.finish_deferred();
    }

    #[kithara::measure(label = "play.decoder.tick")]
    fn tick(&mut self) -> TickResult {
        if let Err(error) = self.synchronize() {
            self.fail(error);
        }
        if self.pending.is_some() {
            return self.admit();
        }
        if self.terminal == Some(Ok(self.source.cursor().segment)) {
            kithara::probe_event!(
                decoder_source_spent,
                segment = self.source.cursor().segment.get()
            );
            return TickResult::Backpressured;
        }
        if matches!(self.terminal, Some(Err(_))) || self.port.forward.is_full() {
            return TickResult::Backpressured;
        }
        let start = WallInstant::now();
        match self.source.step_track() {
            TrackStep::Produced(Fetch::Data { data, source_end }) => {
                self.terminal = None;
                if let Some(load) = &self.engine_load {
                    load.record(
                        start.elapsed(),
                        data.frames(),
                        data.spec().sample_rate.get(),
                    );
                }
                self.pending = Some(PendingPacket {
                    packet: PcmPacket::Chunk(Box::new(data)),
                    source_end,
                });
            }
            TrackStep::Produced(Fetch::NaturalEof) | TrackStep::Eof => {
                self.eof();
            }
            TrackStep::Produced(Fetch::Failure { failure }) => {
                self.fail(failure);
            }
            TrackStep::Failed(error) => {
                self.fail(error);
            }
            TrackStep::StateChanged => {
                self.terminal = None;
                return TickResult::Progress;
            }
            TrackStep::Blocked(reason) => {
                self.source.upstream_parked();
                return match reason {
                    WaitingReason::WaitingDemand => TickResult::UpstreamPending,
                    WaitingReason::Waiting | WaitingReason::WaitingMetadata => TickResult::Waiting,
                };
            }
        }
        self.admit()
    }

    fn warm_up(&mut self) {
        self.source.warm_up();
    }
}

impl<T, S> Drop for DecoderNode<T, S> {
    fn drop(&mut self) {
        if let Some(activity) = &mut self.activity {
            activity.set_playing(false);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_signal::SegmentId;
    use kithara_test_utils::kithara;
    use kithara_worker::{Task, TickResult};

    use crate::{
        mock::{node_fixture::NodeFixture, pcm_fixture::chunk},
        worker::PcmPacket,
    };

    fn pop(fixture: &mut NodeFixture) -> Option<f32> {
        fixture.receiver.pop().map(|packet| {
            let PcmPacket::Chunk(chunk) = packet else {
                panic!("PCM");
            };
            chunk.samples[0]
        })
    }

    fn stage(fixture: &mut NodeFixture, value: f32) {
        fixture.stage(PcmPacket::Chunk(Box::new(chunk(
            SegmentId::FIRST,
            &[value],
        ))));
    }

    #[kithara::test(native, tokio)]
    async fn connect_push_pop() {
        let mut fixture = NodeFixture::new(2).await;
        assert_eq!(pop(&mut fixture), None);
        stage(&mut fixture, 1.0);
        assert_eq!(fixture.node.admit(), TickResult::Progress);
        stage(&mut fixture, 2.0);
        assert_eq!(fixture.node.admit(), TickResult::Progress);
        stage(&mut fixture, 3.0);
        assert_eq!(fixture.node.admit(), TickResult::Backpressured);
        assert_eq!(fixture.node.tick(), TickResult::Backpressured);
        assert!(
            matches!(fixture.node.pending.as_ref().map(|pending| &pending.packet), Some(PcmPacket::Chunk(chunk)) if chunk.samples[0] == 3.0)
        );
        assert_eq!(pop(&mut fixture), Some(1.0));
        assert_eq!(pop(&mut fixture), Some(2.0));
        assert_eq!(pop(&mut fixture), None);
        assert_eq!(fixture.node.admit(), TickResult::Progress);
        assert_eq!(pop(&mut fixture), Some(3.0));
        assert_eq!(pop(&mut fixture), None);
    }

    #[kithara::test(native, tokio)]
    async fn try_push_drains_overflow_first() {
        let mut fixture = NodeFixture::new(1).await;
        stage(&mut fixture, 1.0);
        assert_eq!(fixture.node.admit(), TickResult::Progress);
        stage(&mut fixture, 2.0);
        assert_eq!(fixture.node.admit(), TickResult::Backpressured);
        assert_eq!(pop(&mut fixture), Some(1.0));
        assert_eq!(fixture.node.tick(), TickResult::Progress);
        stage(&mut fixture, 3.0);
        assert_eq!(fixture.node.admit(), TickResult::Backpressured);
        assert_eq!(pop(&mut fixture), Some(2.0));
        assert_eq!(fixture.node.admit(), TickResult::Progress);
        assert_eq!(pop(&mut fixture), Some(3.0));
    }

    #[kithara::test(native, tokio)]
    async fn flush_returns_false_when_ring_full() {
        let mut fixture = NodeFixture::new(1).await;
        stage(&mut fixture, 1.0);
        assert_eq!(fixture.node.admit(), TickResult::Progress);
        stage(&mut fixture, 2.0);
        assert_eq!(fixture.node.admit(), TickResult::Backpressured);
        assert_eq!(fixture.node.admit(), TickResult::Backpressured);
        assert_eq!(pop(&mut fixture), Some(1.0));
        assert_eq!(fixture.node.admit(), TickResult::Progress);
        assert_eq!(pop(&mut fixture), Some(2.0));
        assert_eq!(pop(&mut fixture), None);
    }

    #[kithara::test(native, tokio)]
    async fn direct_push_never_occupies_overflow() {
        let mut fixture = NodeFixture::new(1).await;
        assert!(fixture.node.pending.is_none());
        stage(&mut fixture, 1.0);
        assert_eq!(fixture.node.admit(), TickResult::Progress);
        assert!(fixture.node.pending.is_none());
        assert_eq!(fixture.node.tick(), TickResult::Backpressured);
        assert_eq!(pop(&mut fixture), Some(1.0));
        assert_eq!(pop(&mut fixture), None);
        assert!(fixture.node.pending.is_none());
    }
}
