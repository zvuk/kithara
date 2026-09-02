use kithara_audio::{
    AudioSource, Fetch, PreloadGate, PreparedAudioLane, ProducerPort, TrackStep, WaitingReason,
};
use kithara_events::{AudioEvent, DeferredBus, Event};
use kithara_platform::{
    sync::Arc,
    time::{Duration, Instant},
};
use kithara_signal::AudioChunk;
use kithara_stream::{PlayheadWrite, SeekObserve};
use kithara_test_utils::kithara;
use kithara_worker::{Task, TickResult};

use super::EngineLoad;

/// Per-tick state of a [`DecoderNode`].
#[derive(Default)]
#[non_exhaustive]
pub(super) struct DecoderRuntime {
    pub(super) last_buffer_health_emit: Option<Instant>,
    pub(super) last_engine_load_emit: Option<Instant>,
    pub(super) eof_sent: bool,
    pub(super) preloaded: bool,
    pub(super) seek_epoch: u64,
    pub(super) chunks_sent: usize,
}

/// Play-owned node that drives one still-concrete audio source.
pub(crate) struct DecoderNode<S> {
    emit: Arc<DeferredBus<Event>>,
    playhead: Arc<dyn PlayheadWrite>,
    preload_gate: Arc<PreloadGate>,
    seek_obs: Arc<dyn SeekObserve>,
    runtime: DecoderRuntime,
    engine_load: Option<Arc<EngineLoad>>,
    port: ProducerPort,
    source: S,
    preload_chunks: usize,
}

impl<S> DecoderNode<S> {
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
        if self.runtime.chunks_sent >= self.preload_chunks {
            self.complete_preload();
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

    fn record_load(&self, busy: Duration, fetch: &Fetch<AudioChunk>) {
        if let (Some(load), Fetch::Data { data, .. }) = (self.engine_load.as_ref(), fetch) {
            load.record(busy, data.frames(), data.spec().sample_rate.get());
        }
    }
}

impl<S> DecoderNode<S>
where
    S: AudioSource<Chunk = AudioChunk>,
{
    fn sync_seek_epoch(&mut self) {
        if !self.seek_obs.take_decoder_seek() {
            return;
        }
        let current = self.seek_obs.epoch();
        if current == self.runtime.seek_epoch {
            return;
        }

        self.preload_gate.rearm();
        self.runtime = DecoderRuntime {
            seek_epoch: current,
            ..Default::default()
        };
    }
}

impl<S> DecoderNode<S>
where
    S: AudioSource<Chunk = AudioChunk>,
{
    pub(super) fn new(lane: PreparedAudioLane<S>, engine_load: Option<Arc<EngineLoad>>) -> Self {
        let seek_obs = lane.source.seek_observe();
        let seek_epoch = seek_obs.epoch();
        Self {
            seek_obs,
            engine_load,
            source: lane.source,
            port: lane.port,
            playhead: lane.playhead,
            emit: lane.emit,
            preload_gate: lane.preload_gate,
            preload_chunks: lane.preload_chunks,
            runtime: DecoderRuntime {
                seek_epoch,
                ..Default::default()
            },
        }
    }
}

impl<S> Task for DecoderNode<S>
where
    S: AudioSource<Chunk = AudioChunk>,
{
    fn on_cancel(&mut self) {
        self.complete_preload();
    }

    fn recycle(&mut self) {
        self.port.recycle();
        let _ = self.source.prepare_deferred();
        self.source.finish_deferred();
        self.port.flush_wake();
    }

    #[kithara::measure(label = "play.decoder.tick")]
    #[kithara::rtsan_forbid_blocking]
    fn tick(&mut self) -> TickResult {
        self.sync_seek_epoch();

        if !self.port.can_push_direct() {
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
                let (decoded_frontier, source_end) = match &fetch {
                    Fetch::Data {
                        data,
                        epoch,
                        source_end,
                    } => (
                        Some(data.meta.end_timestamp),
                        source_end.map(|source_end| (source_end, *epoch)),
                    ),
                    _ => (None, None),
                };
                self.port.push_direct(fetch);
                if let Some((source_end, epoch)) = source_end {
                    self.source.commit_source_end(source_end, epoch);
                }
                if let Some(frontier) = decoded_frontier {
                    self.playhead.set_decoded_frontier(frontier);
                }
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
                let marker = Fetch::eof(epoch);
                self.port.push_direct(marker);
                self.complete_preload();
                self.emit
                    .enqueue(AudioEvent::EndOfStream { seek_epoch: epoch }.into());
                self.runtime.eof_sent = true;
                TickResult::Progress
            }

            TrackStep::Failed => {
                let epoch = self.source.decode_epoch();
                let marker = Fetch::failure(epoch);
                self.port.push_direct(marker);
                self.complete_preload();
                TickResult::Done
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
mod scheduler_tests;
#[cfg(test)]
mod tests;
