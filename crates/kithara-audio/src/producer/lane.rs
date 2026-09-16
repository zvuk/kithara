use kithara_events::{DeferredBus, EventSet};
use kithara_platform::sync::Arc;
use kithara_signal::AudioChunk;
use kithara_stream::PlayheadWrite;

use super::PreloadGate;
use crate::{
    AudioEvent, DecoderEvent, Fetch, ScheduledSeekActivator,
    runtime::{Inlet, Outlet},
};

/// Concrete playback output port prepared by `kithara-audio` and driven by
/// the play-owned producer node.
#[doc(hidden)]
pub struct ProducerPort {
    trash_inlet: Inlet<AudioChunk>,
    outlet: Outlet<Fetch<AudioChunk>>,
    scheduled_seek: Option<ScheduledSeekActivator>,
    settled_chunks: usize,
}

impl ProducerPort {
    pub(crate) const fn new(
        outlet: Outlet<Fetch<AudioChunk>>,
        trash_inlet: Inlet<AudioChunk>,
        settled_chunks: usize,
    ) -> Self {
        Self {
            trash_inlet,
            outlet,
            scheduled_seek: None,
            settled_chunks,
        }
    }

    /// Report whether the ring already holds its settled playback depth.
    ///
    /// Capacity past that depth belongs to a replacement epoch that has not
    /// staged its preload, so replacement PCM never waits behind queued PCM.
    #[must_use]
    pub fn holds_settled_depth(&self) -> bool {
        let queued = self.outlet.queued_len();
        queued >= self.settled_chunks
    }

    pub(crate) fn install_scheduled_seek(&mut self, scheduled_seek: ScheduledSeekActivator) {
        self.scheduled_seek = Some(scheduled_seek);
    }

    /// Return the scheduled seek paired with this final playback ring.
    pub fn scheduled_seek(&self) -> Option<&ScheduledSeekActivator> {
        self.scheduled_seek.as_ref()
    }

    /// Reclaim spent chunks outside the checked producer core.
    pub fn recycle(&mut self) {
        while self.trash_inlet.try_pop().is_some() {}
    }

    delegate::delegate! {
        to self.outlet {
            /// Report whether one item can enter the final playback ring directly.
            #[must_use]
            pub fn can_push_direct(&self) -> bool;
            /// Push one produced item directly into the final playback ring.
            pub fn push_direct(&mut self, item: Fetch<AudioChunk>);
            /// Deliver deferred wake signals outside the checked producer core.
            #[call(flush_wake_signals)]
            pub fn flush_wake(&self);
        }
    }
}

/// Worker-neutral playback lane prepared alongside an [`crate::Audio`]
/// reader. `kithara-play` composes the concrete source and owns its node.
#[doc(hidden)]
#[non_exhaustive]
pub struct PreparedAudioLane<S> {
    /// Deferred event publisher shared with the reader.
    pub emit: Arc<DeferredBus<AudioLaneEvent>>,
    /// Canonical playback clock written after final audio admission.
    pub playhead: Arc<dyn PlayheadWrite>,
    /// Gate opened when the final audio ring is preloaded.
    pub preload_gate: Arc<PreloadGate>,
    /// Final output and spent-buffer return port.
    pub port: ProducerPort,
    /// Still-concrete producer source.
    pub source: S,
    /// Number of admitted chunks required before preload completes.
    pub preload_chunks: usize,
}

impl<S> PreparedAudioLane<S> {
    pub(crate) fn map_source_with<A, R, W, F>(
        self,
        auxiliary: A,
        map: F,
    ) -> (R, PreparedAudioLane<W>)
    where
        F: FnOnce(A, S) -> (R, W),
    {
        let (result, source) = map(auxiliary, self.source);
        (
            result,
            PreparedAudioLane {
                source,
                emit: self.emit,
                playhead: self.playhead,
                preload_gate: self.preload_gate,
                port: self.port,
                preload_chunks: self.preload_chunks,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::EventBus;
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_stream::{PlayheadState, SeekControl, SeekObserve, SeekState, WorkerWake};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::audio::SeekHandleParts;

    struct TestWake;

    impl WorkerWake for TestWake {
        fn defer(&self) {}

        fn wake(&self) {}
    }

    #[kithara::test]
    fn scheduled_seek_activates_only_after_old_ring_is_full() {
        let state = Arc::new(SeekState::new());
        let preload_gate = Arc::new(PreloadGate::default());
        let (outlet, mut inlet) = crate::runtime::connect(1, None);
        let (_trash_outlet, trash_inlet) = crate::runtime::connect(3, None);
        let mut port = ProducerPort::new(outlet, trash_inlet, 1);
        port.install_scheduled_seek(ScheduledSeekActivator::new(&SeekHandleParts {
            bus: EventBus::new(8),
            peer_wake: None,
            seek_prepare: None,
            playhead: Arc::new(PlayheadState::new()),
            preload_gate,
            seek: Arc::clone(&state) as Arc<dyn SeekControl>,
            wake: Arc::new(TestWake),
        }));
        let epoch = state.begin_scheduled(Duration::from_secs(5));

        assert_eq!(
            port.scheduled_seek()
                .and_then(|seek| seek.registered_epoch()),
            Some(epoch)
        );
        assert_eq!(state.epoch(), 0);

        port.push_direct(Fetch::eof(0));
        let _ = port.scheduled_seek().map(ScheduledSeekActivator::activate);
        assert_eq!(state.epoch(), epoch);
        assert_eq!(state.target(), Some(Duration::from_secs(5)));
        assert!(matches!(
            inlet.try_pop(),
            Some(Fetch::NaturalEof { epoch: 0 })
        ));
    }
}

/// Event types carried by the decode ring.
#[derive(Clone, Debug, EventSet)]
#[non_exhaustive]
pub enum AudioLaneEvent {
    Decoder(DecoderEvent),
    Audio(AudioEvent),
}
