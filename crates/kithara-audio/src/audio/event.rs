use kithara_decode::PcmMeta;
use kithara_events::{AudioEvent, DeferredBus, EventBus, SeekLifecycleStage, SegmentLocation};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{PlayheadWrite, SeekObserve};

use super::{ReadOutcome, ThreadWake, WakeSignal};

struct Consts;

impl Consts {
    const AUDIO_EVENT_CAPACITY: usize = 64;
    const PROGRESS_EMIT_MIN_DELTA_MS: u64 = 100;
}

pub(super) struct AudioEvents {
    bus: EventBus,
    last_progress_emit: Option<(u64, u64)>,
}

impl AudioEvents {
    pub(super) fn new(bus: EventBus) -> Self {
        Self {
            bus,
            last_progress_emit: None,
        }
    }

    pub(super) fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub(super) fn deferred(bus: &EventBus) -> DeferredBus<AudioEvent> {
        DeferredBus::new(bus.clone(), Consts::AUDIO_EVENT_CAPACITY)
    }

    pub(super) fn emit(&self, event: AudioEvent) {
        self.bus.publish(event);
    }

    pub(super) fn progress(&mut self, playhead: &dyn PlayheadWrite, epoch: u64) {
        let position_ms = clamp_millis(playhead.position());
        if let Some((last_epoch, last_ms)) = self.last_progress_emit
            && last_epoch == epoch
            && position_ms.abs_diff(last_ms) < Consts::PROGRESS_EMIT_MIN_DELTA_MS
        {
            return;
        }
        self.last_progress_emit = Some((epoch, position_ms));

        let total_ms = playhead.duration().map(clamp_millis);
        let decoded_ms = clamp_millis(playhead.decoded_frontier());
        let buffered_ms = Some(total_ms.map_or(decoded_ms, |total| decoded_ms.min(total)));
        self.emit(AudioEvent::PlaybackProgress {
            position_ms,
            total_ms,
            buffered_ms,
            seek_epoch: epoch,
        });
    }

    pub(super) fn post_seek_output(
        &self,
        seek: &dyn SeekObserve,
        epoch: u64,
        meta: Option<PcmMeta>,
        position: Duration,
    ) {
        let Some(seek_epoch) = seek.pending_epoch() else {
            return;
        };
        if seek_epoch != epoch {
            return;
        }

        let variant = meta.as_ref().and_then(|value| value.variant_index);
        let segment_index = meta.as_ref().and_then(|value| value.segment_index);
        self.emit(AudioEvent::SeekLifecycle {
            seek_epoch,
            stage: SeekLifecycleStage::OutputCommitted,
            location: SegmentLocation::new(variant, segment_index, None, None),
        });
        self.emit(AudioEvent::SeekComplete {
            seek_epoch,
            position,
        });
        let _ = seek.clear_pending_epoch(seek_epoch);
    }

    pub(super) fn commit_read(
        &mut self,
        session: &super::core::Session,
        epoch: u64,
        read: super::cursor::CursorRead,
    ) -> ReadOutcome {
        let super::cursor::CursorRead {
            outcome,
            last_output_meta,
        } = read;
        if matches!(outcome, ReadOutcome::Frames { .. }) {
            let position = session.playhead.position();
            self.post_seek_output(session.seek_obs.as_ref(), epoch, last_output_meta, position);
            self.progress(session.playhead.as_ref(), epoch);
        }
        outcome
    }
}

pub(super) struct ReaderOutputWake {
    thread: Arc<ThreadWake>,
    emit: DeferredBus<AudioEvent>,
}

impl ReaderOutputWake {
    pub(super) fn new(thread: Arc<ThreadWake>, emit: DeferredBus<AudioEvent>) -> Self {
        Self { thread, emit }
    }
}

impl WakeSignal for ReaderOutputWake {
    fn wake(&self) {
        WakeSignal::wake(self.thread.as_ref());
    }

    fn on_data_available(&self) {
        self.emit.enqueue(AudioEvent::OutputAvailable);
    }

    fn flush_deferred(&self) {
        self.emit.flush();
    }
}

fn clamp_millis(duration: Duration) -> u64 {
    num_traits::cast::ToPrimitive::to_u64(&duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use kithara_decode::PcmChunk;
    use kithara_events::{AudioEvent, Event, EventBus};
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::audio::{Fetch, ring::create_channels};

    #[kithara::test]
    fn output_available_event_fires_on_empty_to_nonempty_ring_transition() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let reader_wake = Arc::new(ThreadWake::default());
        let (mut tx, mut rx) = create_channels(2, &bus, &reader_wake);

        tx.try_push(Fetch::new(PcmChunk::default(), false, 0))
            .expect("first push reaches ring");
        assert!(events.try_recv().is_err());
        tx.flush_wake_signals();
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Audio(AudioEvent::OutputAvailable))
        ));

        tx.try_push(Fetch::new(PcmChunk::default(), false, 0))
            .expect("second push reaches ring");
        tx.flush_wake_signals();
        assert!(events.try_recv().is_err());

        assert!(rx.try_pop().is_some());
        assert!(rx.try_pop().is_some());

        tx.try_push(Fetch::new(PcmChunk::default(), false, 0))
            .expect("third push reaches empty ring");
        tx.flush_wake_signals();
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Audio(AudioEvent::OutputAvailable))
        ));
    }
}
