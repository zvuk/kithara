//! Bridge between the Queue event stream and FFI observer callbacks.
//!
//! Subscribes to [`Queue::subscribe`] (a unified stream carrying player,
//! audio, hls, file and queue events), translates them into typed
//! [`FfiPlayerEvent`] variants dispatched via a single
//! [`PlayerObserver::on_event`] call. A secondary **OS thread** drives
//! `Queue::tick()` (which pumps `PlayerImpl::tick` and drains engine
//! events for auto-advance) and polls `position_seconds` /
//! `duration_seconds` for periodic time updates — avoiding blocking
//! `Mutex::lock_sync()` inside async.

use std::sync::Arc;

use kithara::play::PlayerEvent;
use kithara_events::{Event, EventReceiver, QueueEvent, TrackStatus};
use kithara_platform::{Duration, JoinHandle, Mutex, sleep, spawn, tokio, tokio::sync::broadcast};
use kithara_queue::Queue;
use tokio_util::sync::CancellationToken;

use crate::{
    observer::{ItemObserver, PlayerObserver},
    player::ItemRegistry,
    types::{FfiItemEvent, FfiItemStatus, FfiPlayerEvent, FfiTrackStatus},
};

pub(crate) struct EventBridge {
    cancel: CancellationToken,
    time_thread: Option<JoinHandle<()>>,
}

impl EventBridge {
    /// Polling interval for time/duration updates (~10 Hz).
    const TIME_POLL_INTERVAL_MS: u64 = 100;

    /// Threshold for suppressing redundant time/duration updates (seconds).
    const TIME_UPDATE_THRESHOLD: f64 = 0.01;

    fn dispatch(
        observer: &Arc<dyn PlayerObserver>,
        queue: &Arc<Queue>,
        items: &Arc<Mutex<ItemRegistry>>,
        last_current: &Mutex<Option<kithara_events::TrackId>>,
        event: &Event,
    ) {
        match event {
            Event::Player(pe) => {
                Self::route_player_event_to_item(items, queue, last_current, pe);
                let Some(ffi_event) = Self::player_event_to_ffi(pe) else {
                    return;
                };
                observer.on_event(ffi_event);
            }
            Event::Queue(qe) => {
                if let QueueEvent::CurrentTrackChanged { id } = qe {
                    let mut prev = last_current.lock_sync();
                    *prev = *id;
                }
                Self::dispatch_queue_event(observer, items, qe);
            }
            _ => {}
        }
    }

    fn dispatch_queue_event(
        observer: &Arc<dyn PlayerObserver>,
        items: &Arc<Mutex<ItemRegistry>>,
        event: &QueueEvent,
    ) {
        match event {
            QueueEvent::CurrentTrackChanged { id } => {
                let item_id = id.and_then(|tid| items.lock_sync().get(&tid).map(|i| i.audio_id()));
                observer.on_event(FfiPlayerEvent::CurrentItemChanged { item_id });
            }
            QueueEvent::TrackStatusChanged { id, status } => {
                let item = items.lock_sync().get(id).cloned();
                if let Some(item) = item {
                    let item_id = item.audio_id();
                    if let Some(item_obs) = item.observer() {
                        Self::route_track_status_to_item(&item_obs, status);
                    }
                    observer.on_event(FfiPlayerEvent::TrackStatusChanged {
                        item_id,
                        status: FfiTrackStatus::from(status.clone()),
                    });
                }
            }
            QueueEvent::QueueEnded => {
                observer.on_event(FfiPlayerEvent::QueueEnded);
            }
            QueueEvent::CrossfadeStarted { duration_seconds } => {
                observer.on_event(FfiPlayerEvent::CrossfadeStarted {
                    duration_seconds: *duration_seconds,
                });
            }
            QueueEvent::CrossfadeDurationChanged { seconds } => {
                observer.on_event(FfiPlayerEvent::CrossfadeDurationChanged { seconds: *seconds });
            }
            _ => {}
        }
    }

    fn player_event_to_ffi(event: &PlayerEvent) -> Option<FfiPlayerEvent> {
        Some(match event {
            PlayerEvent::RateChanged { rate } => FfiPlayerEvent::RateChanged { rate: *rate },
            PlayerEvent::StatusChanged { status } => FfiPlayerEvent::StatusChanged {
                status: (*status).into(),
            },
            PlayerEvent::TimeControlStatusChanged { status, .. } => {
                FfiPlayerEvent::TimeControlStatusChanged {
                    status: (*status).into(),
                }
            }
            PlayerEvent::VolumeChanged { volume } => {
                FfiPlayerEvent::VolumeChanged { volume: *volume }
            }
            PlayerEvent::MuteChanged { muted } => FfiPlayerEvent::MuteChanged { muted: *muted },
            PlayerEvent::ItemDidPlayToEnd { .. } => FfiPlayerEvent::ItemDidPlayToEnd,
            _ => return None,
        })
    }

    /// Forward player-level signals (`ItemDidPlayToEnd`,
    /// `TimeControlStatusChanged → WaitingToPlay`) to the corresponding
    /// item-level observer, mapping them onto
    /// [`FfiItemEvent::DidReachEnd`] / [`FfiItemEvent::DidStall`].
    fn route_player_event_to_item(
        items: &Arc<Mutex<ItemRegistry>>,
        queue: &Arc<Queue>,
        last_current: &Mutex<Option<kithara_events::TrackId>>,
        event: &PlayerEvent,
    ) {
        let target = match event {
            PlayerEvent::ItemDidPlayToEnd { .. } => *last_current.lock_sync(),
            PlayerEvent::TimeControlStatusChanged {
                status: kithara::play::TimeControlStatus::WaitingToPlay,
                ..
            } => queue.current().map(|entry| entry.id),
            _ => return,
        };
        let Some(track_id) = target else { return };
        let Some(item) = items.lock_sync().get(&track_id).cloned() else {
            return;
        };
        let Some(item_obs) = item.observer() else {
            return;
        };
        let ffi_event = match event {
            PlayerEvent::ItemDidPlayToEnd { .. } => FfiItemEvent::DidReachEnd,
            PlayerEvent::TimeControlStatusChanged { .. } => FfiItemEvent::DidStall,
            _ => return,
        };
        item_obs.on_event(ffi_event);
    }

    /// Translate a queue-level `TrackStatus` into per-item callbacks so
    /// Swift `KitharaPlayerItem.eventPublisher` sees `StatusChanged` +
    /// `Error` without having to subscribe to the player-level stream.
    fn route_track_status_to_item(observer: &Arc<dyn ItemObserver>, status: &TrackStatus) {
        match status {
            TrackStatus::Loaded => {
                observer.on_event(FfiItemEvent::StatusChanged {
                    status: FfiItemStatus::ReadyToPlay,
                });
            }
            TrackStatus::Failed(reason) => {
                observer.on_event(FfiItemEvent::StatusChanged {
                    status: FfiItemStatus::Failed,
                });
                observer.on_event(FfiItemEvent::Error {
                    error: reason.clone(),
                });
            }
            _ => {}
        }
    }

    /// Spawn background tasks that translate queue/player events into
    /// observer callbacks. Returns a bridge handle; dropping it cancels
    /// the tasks.
    pub(crate) fn spawn(
        rx: EventReceiver,
        observer: Arc<dyn PlayerObserver>,
        queue: Arc<Queue>,
        items: &Arc<Mutex<ItemRegistry>>,
        cancel: CancellationToken,
    ) -> Self {
        let last_current = Arc::new(Mutex::new(None));
        Self::spawn_event_task(
            rx,
            Arc::clone(&observer),
            Arc::clone(&queue),
            Arc::clone(items),
            Arc::clone(&last_current),
            cancel.clone(),
        );
        let time_thread = Self::spawn_time_thread(queue, observer, cancel.clone());
        Self {
            cancel,
            time_thread: Some(time_thread),
        }
    }

    /// Task that listens for queue events on the unified bus.
    fn spawn_event_task(
        mut rx: EventReceiver,
        observer: Arc<dyn PlayerObserver>,
        queue: Arc<Queue>,
        items: Arc<Mutex<ItemRegistry>>,
        last_current: Arc<Mutex<Option<kithara_events::TrackId>>>,
        cancel: CancellationToken,
    ) {
        crate::FFI_RUNTIME.spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    event = rx.recv() => {
                        match event {
                            Ok(ev) => Self::dispatch(
                                &observer,
                                &queue,
                                &items,
                                &last_current,
                                &ev,
                            ),
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
    }

    /// Dedicated OS thread that drives `Queue::tick` and polls current
    /// time / duration at ~10 Hz. Uses a plain thread instead of an
    /// async task to avoid blocking the single-threaded tokio runtime
    /// with sync locks held inside the engine.
    fn spawn_time_thread(
        queue: Arc<Queue>,
        observer: Arc<dyn PlayerObserver>,
        cancel: CancellationToken,
    ) -> JoinHandle<()> {
        spawn(move || {
            let interval = Duration::from_millis(Self::TIME_POLL_INTERVAL_MS);
            let mut last_time: Option<f64> = None;
            let mut last_duration: Option<f64> = None;

            while !cancel.is_cancelled() {
                sleep(interval);
                let _ = queue.tick();
                queue.process_notifications();
                let time = queue.position_seconds();
                let duration = queue.duration_seconds();

                match time {
                    Some(t)
                        if last_time
                            .is_none_or(|prev| (prev - t).abs() > Self::TIME_UPDATE_THRESHOLD) =>
                    {
                        observer.on_event(FfiPlayerEvent::TimeChanged { seconds: t });
                        last_time = Some(t);
                    }
                    None if last_time.is_some() => {
                        last_time = None;
                    }
                    _ => {}
                }

                match duration {
                    Some(d)
                        if last_duration
                            .is_none_or(|prev| (prev - d).abs() > Self::TIME_UPDATE_THRESHOLD) =>
                    {
                        observer.on_event(FfiPlayerEvent::DurationChanged { seconds: d });
                        last_duration = Some(d);
                    }
                    None if last_duration.is_some() => {
                        last_duration = None;
                    }
                    _ => {}
                }
            }
        })
    }
}

impl Drop for EventBridge {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.time_thread.take() {
            handle.join().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[kithara::test]
    fn event_bridge_is_send() {
        assert_send::<EventBridge>();
    }
}
