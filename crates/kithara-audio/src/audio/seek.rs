use kithara_events::EventBus;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{
    DeferredWake, PlayheadWrite, ScheduledSeekActivation, SeekControl, SeekPrepare,
};
use tracing::trace;

use super::{PreloadGate, SeekOutcome};
use crate::{AudioEvent, ScheduledSeek, SeekLifecycleStage, SegmentLocation, traits::SeekBegin};

/// The control-plane half of a seek: rebuilds the source's byte space, publishes a lifecycle event,
/// nudges the peer and wakes the worker. Each takes a lock, so the audio thread only runs
/// [`Audio::sync_seek`](super::Audio::sync_seek).
pub struct SeekHandle {
    playhead: Arc<dyn PlayheadWrite>,
    preload_gate: Arc<PreloadGate>,
    seek: Arc<dyn SeekControl>,
    wake: Arc<dyn kithara_stream::WorkerWake>,
    bus: EventBus,
    peer_wake: Option<Arc<DeferredWake>>,
    seek_prepare: Option<Arc<dyn SeekPrepare>>,
}

/// Worker-side half of scheduled seek publication.
#[doc(hidden)]
#[derive(Clone)]
pub struct ScheduledSeekActivator {
    preload_gate: Arc<PreloadGate>,
    seek: Arc<dyn SeekControl>,
    wake: Arc<dyn kithara_stream::WorkerWake>,
    bus: EventBus,
    peer_wake: Option<Arc<DeferredWake>>,
    seek_prepare: Option<Arc<dyn SeekPrepare>>,
}

impl ScheduledSeekActivator {
    pub(crate) fn new(parts: &SeekHandleParts) -> Self {
        Self {
            preload_gate: Arc::clone(&parts.preload_gate),
            seek: Arc::clone(&parts.seek),
            wake: Arc::clone(&parts.wake),
            bus: parts.bus.clone(),
            peer_wake: parts.peer_wake.clone(),
            seek_prepare: parts.seek_prepare.clone(),
        }
    }

    /// Publish the registered seek after the producer has filled the old PCM ring.
    #[must_use]
    pub fn activate(&self) -> ScheduledSeekActivation {
        let activation = self.seek.activate_scheduled();
        let ScheduledSeekActivation::Activated { epoch } = activation else {
            return activation;
        };
        kithara_test_macros::probe_event!(scheduled_seek_activated, seek_epoch = epoch);
        if let Some(prepare) = &self.seek_prepare {
            prepare.prepare();
        }
        self.bus.publish(AudioEvent::SeekLifecycle {
            seek_epoch: epoch,
            stage: SeekLifecycleStage::SeekRequest,
            location: SegmentLocation::default(),
        });
        if let Some(wake) = &self.peer_wake {
            wake.notify_now();
        }
        self.preload_gate.rearm();
        self.wake.wake();
        activation
    }

    /// Return the exact registered request awaiting worker-side activation.
    #[must_use]
    pub fn registered_epoch(&self) -> Option<u64> {
        self.seek.scheduled_epoch()
    }
}

impl SeekHandle {
    pub(super) fn new(parts: SeekHandleParts) -> Self {
        let SeekHandleParts {
            bus,
            peer_wake,
            playhead,
            preload_gate,
            seek,
            seek_prepare,
            wake,
        } = parts;
        Self {
            playhead,
            preload_gate,
            seek,
            wake,
            bus,
            peer_wake,
            seek_prepare,
        }
    }

    fn begin_inner(&self, position: Duration, present: bool) -> ScheduledSeek {
        if let Some(prepare) = &self.seek_prepare {
            prepare.prepare();
        }
        let epoch = self.seek.begin(position);
        if present {
            self.seek.mark_pending(epoch);
        }
        self.bus.publish(AudioEvent::SeekLifecycle {
            seek_epoch: epoch,
            stage: SeekLifecycleStage::SeekRequest,
            location: SegmentLocation::default(),
        });
        if let Some(wake) = &self.peer_wake {
            wake.notify_now();
        }
        self.preload_gate.rearm();
        self.wake.wake();

        trace!(?position, epoch, present, "seek begun");
        let outcome = match self.playhead.duration() {
            Some(duration) if position >= duration => SeekOutcome::PastEof {
                duration,
                target: position,
            },
            _ => SeekOutcome::Landed {
                target: position,
                landed_at: position,
            },
        };
        ScheduledSeek { epoch, outcome }
    }
}

impl SeekBegin for SeekHandle {
    fn begin(&self, position: Duration) -> SeekOutcome {
        self.begin_inner(position, true).outcome
    }

    fn begin_prepared(&self, position: Duration) -> ScheduledSeek {
        self.begin_inner(position, false)
    }

    fn begin_scheduled(&self, position: Duration) -> ScheduledSeek {
        let epoch = self.seek.begin_scheduled(position);
        self.wake.wake();
        let outcome = match self.playhead.duration() {
            Some(duration) if position >= duration => SeekOutcome::PastEof {
                duration,
                target: position,
            },
            _ => SeekOutcome::Landed {
                target: position,
                landed_at: position,
            },
        };
        ScheduledSeek { epoch, outcome }
    }
}

pub(crate) struct SeekHandleParts {
    pub(crate) playhead: Arc<dyn PlayheadWrite>,
    pub(crate) preload_gate: Arc<PreloadGate>,
    pub(crate) seek: Arc<dyn SeekControl>,
    pub(crate) wake: Arc<dyn kithara_stream::WorkerWake>,
    pub(crate) bus: EventBus,
    pub(crate) peer_wake: Option<Arc<DeferredWake>>,
    pub(crate) seek_prepare: Option<Arc<dyn SeekPrepare>>,
}
