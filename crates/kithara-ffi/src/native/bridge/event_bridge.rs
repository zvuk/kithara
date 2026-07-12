use kithara::play::{PlayerEvent, TimeControlStatus};
use kithara_events::{Envelope, Event, EventReceiver, QueueEvent, TrackId, TrackStatus};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    thread::{JoinHandle, sleep, spawn},
    time::Duration,
    tokio,
    tokio::sync::broadcast,
};
use kithara_queue::Queue;

use crate::{
    observer::{ItemObserver, PlayerObserver},
    registry::ItemRegistry,
    types::{FfiItemEvent, FfiItemStatus, FfiPlayerEvent, FfiTimeRange, FfiTrackStatus},
};

pub(crate) struct EventBridge {
    cancel: CancelToken,
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
        last_current: &Mutex<Option<TrackId>>,
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
                    let mut prev = last_current.lock();
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
                let item_id = *id;
                observer.on_event(FfiPlayerEvent::CurrentItemChanged { item_id });
            }
            QueueEvent::TrackStatusChanged { id, status } => {
                let Some(item) = items.lock().get(id).cloned() else {
                    return;
                };
                if let Some(item_obs) = item.observer() {
                    Self::route_track_status_to_item(&item_obs, status);
                }
                observer.on_event(FfiPlayerEvent::TrackStatusChanged {
                    item_id: *id,
                    status: FfiTrackStatus::from(status.clone()),
                });
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

    /// Emit `make_event(value)` when `value` differs from `last` by more
    /// than [`Self::TIME_UPDATE_THRESHOLD`], tracking the last emitted
    /// value (and clearing it when the source goes empty).
    fn emit_if_changed(
        observer: &Arc<dyn PlayerObserver>,
        value: Option<f64>,
        last: &mut Option<f64>,
        make_event: impl FnOnce(f64) -> FfiPlayerEvent,
    ) {
        match value {
            Some(v) if last.is_none_or(|prev| (prev - v).abs() > Self::TIME_UPDATE_THRESHOLD) => {
                observer.on_event(make_event(v));
                *last = Some(v);
            }
            None if last.is_some() => *last = None,
            _ => {}
        }
    }

    /// Push refreshed loaded ranges to the current item's observer when the
    /// polled frontier moves. Using the polled decoded frontier (not the
    /// lossy byte-ratio telemetry that under-reported a VBR-FLAC quiet
    /// intro) keeps loaded ranges always covering the playhead, so the
    /// host never wrongly pauses into a buffering deadlock.
    fn emit_loaded_ranges(
        items: &Arc<Mutex<ItemRegistry>>,
        last_current: &Mutex<Option<TrackId>>,
        frontier: Option<f64>,
        last: &mut Option<f64>,
    ) {
        let Some(frontier) = frontier else {
            *last = None;
            return;
        };
        if last.is_some_and(|prev| (prev - frontier).abs() <= Self::TIME_UPDATE_THRESHOLD) {
            return;
        }
        let Some(track_id) = *last_current.lock() else {
            return;
        };
        let Some(item) = items.lock().get(&track_id).cloned() else {
            return;
        };
        let Some(item_obs) = item.observer() else {
            return;
        };
        *last = Some(frontier);
        item_obs.on_event(FfiItemEvent::LoadedRangesChanged {
            ranges: Self::loaded_ranges_from_frontier(frontier),
        });
    }

    /// Build loaded ranges from the decoded-ahead frontier.
    ///
    /// The frontier is the authoritative buffered/playable window (always
    /// `>=` the playhead), so it is reported as a single range `[0,
    /// frontier]`. An empty vec means nothing is decoded yet.
    fn loaded_ranges_from_frontier(frontier: f64) -> Vec<FfiTimeRange> {
        if frontier > 0.0 {
            vec![FfiTimeRange {
                start_seconds: 0.0,
                duration_seconds: frontier,
            }]
        } else {
            Vec::new()
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
            PlayerEvent::ItemDidFail { item_id, .. } => FfiPlayerEvent::ItemDidFail {
                item_id: item_id
                    .as_ref()
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(TrackId::from),
            },
            _ => return None,
        })
    }

    /// Forward player-level signals (`ItemDidPlayToEnd`, `ItemDidFail`,
    /// `TimeControlStatusChanged → WaitingToPlay`) to the corresponding
    /// item-level observer, mapping them onto
    /// [`FfiItemEvent::DidReachEnd`] / [`FfiItemEvent::DidFail`] /
    /// [`FfiItemEvent::DidStall`].
    fn route_player_event_to_item(
        items: &Arc<Mutex<ItemRegistry>>,
        queue: &Arc<Queue>,
        last_current: &Mutex<Option<TrackId>>,
        event: &PlayerEvent,
    ) {
        let target = match event {
            PlayerEvent::ItemDidPlayToEnd { .. } | PlayerEvent::ItemDidFail { .. } => {
                *last_current.lock()
            }
            PlayerEvent::TimeControlStatusChanged {
                status: TimeControlStatus::WaitingToPlay,
                ..
            } => queue.current().map(|entry| entry.id),
            _ => return,
        };
        let Some(track_id) = target else { return };
        let Some(item) = items.lock().get(&track_id).cloned() else {
            return;
        };
        let Some(item_obs) = item.observer() else {
            return;
        };
        let ffi_event = match event {
            PlayerEvent::ItemDidPlayToEnd { .. } => FfiItemEvent::DidReachEnd,
            PlayerEvent::ItemDidFail { .. } => FfiItemEvent::DidFail,
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
        cancel: CancelToken,
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
        let time_thread = Self::spawn_time_thread(
            queue,
            observer,
            Arc::clone(items),
            last_current,
            cancel.clone(),
        );
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
        last_current: Arc<Mutex<Option<TrackId>>>,
        cancel: CancelToken,
    ) {
        crate::FFI_RUNTIME.spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    event = rx.recv() => {
                        match event {
                            Ok(Envelope { event: ev, .. }) => Self::dispatch(
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
    /// time / duration / decoded frontier at ~10 Hz. Uses a plain thread
    /// instead of an async task to avoid blocking the single-threaded
    /// tokio runtime with sync locks held inside the engine.
    fn spawn_time_thread(
        queue: Arc<Queue>,
        observer: Arc<dyn PlayerObserver>,
        items: Arc<Mutex<ItemRegistry>>,
        last_current: Arc<Mutex<Option<TrackId>>>,
        cancel: CancelToken,
    ) -> JoinHandle<()> {
        spawn(move || {
            let interval = Duration::from_millis(Self::TIME_POLL_INTERVAL_MS);
            let mut last_time: Option<f64> = None;
            let mut last_duration: Option<f64> = None;
            let mut last_buffered: Option<f64> = None;

            while !cancel.is_cancelled() {
                sleep(interval);
                let _ = queue.tick();
                queue.process_notifications();
                let view = queue.playback_view();
                Self::emit_if_changed(&observer, view.position, &mut last_time, |seconds| {
                    FfiPlayerEvent::TimeChanged { seconds }
                });
                Self::emit_if_changed(&observer, view.duration, &mut last_duration, |seconds| {
                    FfiPlayerEvent::DurationChanged { seconds }
                });
                Self::emit_loaded_ranges(&items, &last_current, view.buffered, &mut last_buffered);
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
    use crate::{item::AudioPlayerItem, types::FfiItemConfig};

    fn assert_send<T: Send>() {}

    fn item_config() -> FfiItemConfig {
        FfiItemConfig {
            abr_mode: None,
            audio_id: None,
            headers: None,
            uuid_i64: None,
            url: "https://example.com/quiet-intro.flac".to_string(),
            is_live_stream: false,
            preferred_peak_bitrate: 0.0,
            preferred_peak_bitrate_expensive: 0.0,
        }
    }

    #[kithara::test]
    fn event_bridge_is_send() {
        assert_send::<EventBridge>();
    }

    /// The decoded-ahead frontier covers the playhead, so loaded ranges
    /// built from it keep the item playable — unlike the old byte-ratio
    /// telemetry that under-reported a VBR-FLAC quiet intro (~0.66s decoded
    /// byte-ratio at a 0.917s playhead) and made the host pause into a
    /// buffering deadlock.
    #[kithara::test]
    fn loaded_ranges_from_frontier_cover_playhead() {
        let item = AudioPlayerItem::new(item_config());
        let ranges = EventBridge::loaded_ranges_from_frontier(4.0);
        assert!(item.is_playable(0.917, ranges));
    }

    #[kithara::test]
    fn loaded_ranges_empty_when_nothing_decoded() {
        assert!(EventBridge::loaded_ranges_from_frontier(0.0).is_empty());
    }
}
