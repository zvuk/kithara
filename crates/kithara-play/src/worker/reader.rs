use std::num::NonZeroU32;

use kithara_audio::{
    Audio, AudioControl, AudioRead, AudioSession, ChunkOutcome, PreloadGate, ReadOutcome,
    SeekBegin, SeekOutcome,
};
use kithara_decode::{DecodeError, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{maybe_send::MaybeSend, sync::Arc, time::Duration};
use kithara_signal::AudioSpec;
use kithara_warp::{RenderPublisher, Warp};
use kithara_worker::{TaskControl, TaskHandle};

use super::{PlayWorker, scheduler::ServiceClass};

#[derive(Clone)]
pub(crate) struct TrackPriority {
    control: TaskControl,
}

impl TrackPriority {
    pub(super) const fn new(control: TaskControl) -> Self {
        Self { control }
    }

    pub(crate) fn set(&self, class: ServiceClass) {
        self.control.set_priority(class.into());
    }
}

pub(crate) struct TrackLease<S> {
    task: TaskHandle,
    _worker: PlayWorker<S>,
}

impl<S> TrackLease<S> {
    pub(crate) const fn new(worker: PlayWorker<S>, task: TaskHandle) -> Self {
        Self {
            task,
            _worker: worker,
        }
    }

    pub(crate) fn priority(&self) -> TrackPriority {
        TrackPriority::new(self.task.control())
    }
}

/// Audio reader whose producer task is registered with a [`PlayWorker`].
///
/// The reader drops before its registration lease, so its wake handles and
/// buffers are released before the final worker owner can shut down.
pub struct RegisteredAudio<T, S> {
    warp: Warp<Audio<T>>,
    _lease: TrackLease<S>,
}

impl<T, S> RegisteredAudio<T, S> {
    pub(super) const fn new(warp: Warp<Audio<T>>, lease: TrackLease<S>) -> Self {
        Self {
            warp,
            _lease: lease,
        }
    }

    pub(crate) fn priority(&self) -> TrackPriority {
        self._lease.priority()
    }

    pub(crate) fn publisher(&self) -> RenderPublisher {
        self.warp.publisher()
    }
}

impl<T: MaybeSend, S> AudioRead for RegisteredAudio<T, S> {
    delegate::delegate! {
        to self.warp.source() {
            fn cached_span(&self) -> Duration;
            fn decoded_frontier(&self) -> Duration;
            fn position(&self) -> Duration;
            fn spec(&self) -> AudioSpec;
        }
        to self.warp.source_mut() {
            fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError>;
            fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError>;
            fn read_planar<'a>(
                &mut self,
                output: &'a mut [&'a mut [f32]],
            ) -> Result<ReadOutcome, DecodeError>;
        }
    }
}

impl<T: MaybeSend, S> AudioSession for RegisteredAudio<T, S> {
    delegate::delegate! {
        to self.warp.source() {
            fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            fn duration(&self) -> Option<Duration>;
            fn event_bus(&self) -> &EventBus;
            fn is_preloaded(&self) -> bool;
            fn metadata(&self) -> &TrackMetadata;
            fn preload_epoch(&self) -> u64;
            fn preload_gate(&self) -> Option<Arc<PreloadGate>>;
        }
    }
}

impl<T: MaybeSend, S> AudioControl for RegisteredAudio<T, S> {
    delegate::delegate! {
        to self.warp.source_mut() {
            fn preload(&mut self) -> Result<(), DecodeError>;
            fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError>;
            fn sync_seek(&mut self);
        }
        to self.warp.source() {
            fn set_host_sample_rate(&self, sample_rate: NonZeroU32);
        }
    }

    fn seek_handle(&self) -> Option<Arc<dyn SeekBegin>> {
        AudioControl::seek_handle(self.warp.source())
    }
}
