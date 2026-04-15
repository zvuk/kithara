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

use std::{collections::HashMap, sync::Arc};

use kithara::play::PlayerEvent;
use kithara_events::{Event, EventReceiver, QueueEvent, TrackId};
use kithara_platform::{Duration, JoinHandle, Mutex, sleep, spawn, tokio, tokio::sync::broadcast};
use kithara_queue::Queue;
use tokio_util::sync::CancellationToken;

use crate::{item::AudioPlayerItem, observer::PlayerObserver, types::FfiPlayerEvent};

/// Polling interval for time/duration updates (~10 Hz).
const TIME_POLL_INTERVAL_MS: u64 = 100;

/// Threshold for suppressing redundant time/duration updates (seconds).
const TIME_UPDATE_THRESHOLD: f64 = 0.01;

/// Forwards Queue/Player/Audio/Hls/File events to an observer on
/// background tasks.
pub(crate) struct EventBridge {
    cancel: CancellationToken,
    time_thread: Option<JoinHandle<()>>,
}

impl EventBridge {
    /// Spawn background tasks that translate queue/player events into
    /// observer callbacks. Returns a bridge handle; dropping it cancels
    /// the tasks.
    pub(crate) fn spawn(
        rx: EventReceiver,
        observer: Arc<dyn PlayerObserver>,
        queue: Arc<Queue>,
        items: &Arc<Mutex<HashMap<TrackId, Arc<AudioPlayerItem>>>>,
        cancel: CancellationToken,
    ) -> Self {
        Self::spawn_event_task(rx, Arc::clone(&observer), Arc::clone(items), cancel.clone());
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
        items: Arc<Mutex<HashMap<TrackId, Arc<AudioPlayerItem>>>>,
        cancel: CancellationToken,
    ) {
        crate::FFI_RUNTIME.spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    event = rx.recv() => {
                        match event {
                            Ok(ev) => Self::dispatch(&observer, &items, &ev),
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
            let interval = Duration::from_millis(TIME_POLL_INTERVAL_MS);
            let mut last_time: Option<f64> = None;
            let mut last_duration: Option<f64> = None;

            while !cancel.is_cancelled() {
                sleep(interval);
                // Pump engine updates and drain `ItemDidPlayToEnd` /
                // `CurrentItemChanged` into QueueEvents for consistent
                // handling below.
                let _ = queue.tick();
                queue.player().process_notifications();
                let time = queue.player().position_seconds();
                let duration = queue.player().duration_seconds();

                match time {
                    Some(t)
                        if last_time
                            .is_none_or(|prev| (prev - t).abs() > TIME_UPDATE_THRESHOLD) =>
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
                            .is_none_or(|prev| (prev - d).abs() > TIME_UPDATE_THRESHOLD) =>
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

    fn dispatch(
        observer: &Arc<dyn PlayerObserver>,
        items: &Arc<Mutex<HashMap<TrackId, Arc<AudioPlayerItem>>>>,
        event: &Event,
    ) {
        match event {
            Event::Player(pe) => {
                let Some(ffi_event) = Self::player_event_to_ffi(pe) else {
                    return;
                };
                observer.on_event(ffi_event);
            }
            Event::Queue(QueueEvent::CurrentTrackChanged { id }) => {
                let item_id = id.and_then(|tid| items.lock_sync().get(&tid).map(|i| i.id()));
                observer.on_event(FfiPlayerEvent::CurrentItemChanged { item_id });
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
            PlayerEvent::ItemDidPlayToEnd => FfiPlayerEvent::ItemDidPlayToEnd,
            // `PlayerEvent::CurrentItemChanged` is shadowed by
            // `QueueEvent::CurrentTrackChanged` (carries the item id).
            _ => return None,
        })
    }
}

impl Drop for EventBridge {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.time_thread.take() {
            let _ = handle.join();
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
