use kithara::{
    events::{Envelope, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    platform::{
        CancelToken,
        sync::{Arc, Mutex},
        thread::{JoinHandle, sleep, spawn},
        time::Duration,
        tokio,
        tokio::sync::broadcast,
    },
    play::{PlayerEvent, TimeControlStatus},
};

use crate::{
    item::AudioPlayerItem,
    observer::{ItemObserver, PlayerObserver},
    pools::FfiQueueControl,
    registry::ItemRegistry,
    types::{
        FfiAdvanceReason, FfiItemEvent, FfiItemStatus, FfiPlayerEvent, FfiRepeatMode, FfiTimeRange,
        FfiTrackStatus,
    },
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
        items: &Arc<Mutex<ItemRegistry>>,
        last_current: &Mutex<Option<TrackId>>,
        event: &Event,
    ) {
        if let Event::Player(pe) = event {
            Self::route_player_event_to_item(items, last_current, pe);
            let Some(ffi_event) = FfiPlayerEvent::try_from(pe).ok() else {
                return;
            };
            observer.on_event(ffi_event);
            return;
        }
        if let Event::Queue(qe) = event {
            if let QueueEvent::CurrentTrackChanged { id } = qe {
                let mut prev = last_current.lock();
                *prev = *id;
            }
            Self::dispatch_queue_event(observer, items, qe);
            return;
        }
        if let Ok(ffi_event) = FfiPlayerEvent::try_from(event) {
            observer.on_event(ffi_event);
        }
    }

    fn dispatch_queue_event(
        observer: &Arc<dyn PlayerObserver>,
        items: &Arc<Mutex<ItemRegistry>>,
        event: &QueueEvent,
    ) {
        match event {
            QueueEvent::TrackAdded { id, index } => {
                observer.on_event(FfiPlayerEvent::TrackAdded {
                    item_id: *id,
                    index: *index as u64,
                });
            }
            QueueEvent::TrackRemoved { id } => {
                observer.on_event(FfiPlayerEvent::TrackRemoved { item_id: *id });
            }
            QueueEvent::CurrentTrackChanged { id } => {
                let item_id = *id;
                observer.on_event(FfiPlayerEvent::CurrentItemChanged { item_id });
            }
            QueueEvent::CurrentTrackAdvance { id, reason } => {
                observer.on_event(FfiPlayerEvent::CurrentItemAdvanced {
                    item_id: *id,
                    reason: FfiAdvanceReason::from(*reason),
                });
            }
            QueueEvent::TrackStatusChanged { id, status } => {
                let Some(item) = items.lock().get(id).cloned() else {
                    return;
                };
                if let Some(item_obs) = item.observer() {
                    Self::route_track_status_to_item(&item, &item_obs, status);
                }
                observer.on_event(FfiPlayerEvent::TrackStatusChanged {
                    item_id: *id,
                    status: FfiTrackStatus::from(status.clone()),
                });
            }
            QueueEvent::QueueEnded => {
                observer.on_event(FfiPlayerEvent::QueueEnded);
            }
            QueueEvent::TrackLoadFailed {
                id,
                reason,
                auto_skipped,
            } => {
                observer.on_event(FfiPlayerEvent::TrackLoadFailed {
                    item_id: *id,
                    reason: reason.clone(),
                    auto_skipped: *auto_skipped,
                });
            }
            QueueEvent::CrossfadeStarted { duration_seconds } => {
                observer.on_event(FfiPlayerEvent::CrossfadeStarted {
                    duration_seconds: *duration_seconds,
                });
            }
            QueueEvent::CrossfadeDurationChanged { seconds } => {
                observer.on_event(FfiPlayerEvent::CrossfadeDurationChanged { seconds: *seconds });
            }
            QueueEvent::RepeatModeChanged { mode } => {
                observer.on_event(FfiPlayerEvent::RepeatModeChanged {
                    mode: FfiRepeatMode::from(*mode),
                });
            }
            QueueEvent::NextTrackReady { id, index } => {
                observer.on_event(FfiPlayerEvent::NextTrackReady {
                    item_id: *id,
                    index: *index as u64,
                });
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
    /// polled buffered window moves. The window is the queue view's union of
    /// the cached span and the decoded frontier: `loadedTimeRanges` means
    /// "available without more network", which is the cached span, while the
    /// frontier stays a floor so the reported window never falls behind the
    /// playhead and pushes the host into a buffering deadlock.
    fn emit_loaded_ranges(
        items: &Arc<Mutex<ItemRegistry>>,
        last_current: &Mutex<Option<TrackId>>,
        available: Option<f64>,
        last: &mut Option<f64>,
    ) {
        let Some(available) = available else {
            *last = None;
            return;
        };
        if last.is_some_and(|prev| (prev - available).abs() <= Self::TIME_UPDATE_THRESHOLD) {
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
        *last = Some(available);
        item_obs.on_event(FfiItemEvent::LoadedRangesChanged {
            ranges: Self::loaded_ranges(available),
        });
    }

    /// Build loaded ranges from the available window.
    ///
    /// Reported as a single range `[0, available]`. An empty vec means
    /// nothing is available yet.
    fn loaded_ranges(available: f64) -> Vec<FfiTimeRange> {
        if available > 0.0 {
            vec![FfiTimeRange {
                start_seconds: 0.0,
                duration_seconds: available,
            }]
        } else {
            Vec::new()
        }
    }

    /// Forward player-level signals (`ItemDidPlayToEnd`, `ItemDidFail`,
    /// `TimeControlStatusChanged → WaitingToPlay`) to the corresponding
    /// item-level observer, mapping them onto
    /// [`FfiItemEvent::DidReachEnd`] / [`FfiItemEvent::DidFail`] /
    /// [`FfiItemEvent::DidStall`].
    fn route_player_event_to_item(
        items: &Arc<Mutex<ItemRegistry>>,
        last_current: &Mutex<Option<TrackId>>,
        event: &PlayerEvent,
    ) {
        let target = match event {
            PlayerEvent::ItemDidPlayToEnd { item } | PlayerEvent::ItemDidFail { item } => {
                Some(item.id())
            }
            PlayerEvent::TimeControlStatusChanged {
                status: TimeControlStatus::WaitingToPlay,
                ..
            } => *last_current.lock(),
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

    /// Forward queue settlement to the item observer. An item reports its
    /// terminal pair once: whichever source settles it first emits, and the
    /// other finds it already failed and stays silent. A protocol failure
    /// reaches the item through [`crate::native::item_bridge::ItemEventBridge`]
    /// carrying the exact reason, and usually arrives first; a queue failure
    /// with no protocol event behind it — a decode, DRM, or storage refusal —
    /// still reaches the item from here.
    fn route_track_status_to_item(
        item: &AudioPlayerItem,
        observer: &Arc<dyn ItemObserver>,
        status: &TrackStatus,
    ) {
        if matches!(status, TrackStatus::Loaded) {
            observer.on_event(FfiItemEvent::StatusChanged {
                status: FfiItemStatus::ReadyToPlay,
            });
            return;
        }
        let TrackStatus::Failed(reason) = status else {
            return;
        };
        if !item.state.lock().mark_failed() {
            return;
        }
        observer.on_event(FfiItemEvent::StatusChanged {
            status: FfiItemStatus::Failed,
        });
        observer.on_event(FfiItemEvent::Error {
            error: reason.clone(),
        });
    }

    /// Spawn background tasks that translate queue/player events into
    /// observer callbacks. Returns a bridge handle; dropping it cancels
    /// the tasks.
    pub(crate) fn spawn(
        rx: EventReceiver,
        observer: Arc<dyn PlayerObserver>,
        queue: FfiQueueControl,
        items: &Arc<Mutex<ItemRegistry>>,
        cancel: CancelToken,
    ) -> Self {
        let last_current = Arc::new(Mutex::new(None));
        Self::spawn_event_task(
            rx,
            Arc::clone(&observer),
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
        queue: FfiQueueControl,
        observer: Arc<dyn PlayerObserver>,
        items: Arc<Mutex<ItemRegistry>>,
        last_current: Arc<Mutex<Option<TrackId>>>,
        cancel: CancelToken,
    ) -> JoinHandle<()> {
        spawn(move || {
            let _rt = crate::FFI_RUNTIME.enter();
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
    use std::sync::{Condvar, Mutex as StdMutex, PoisonError};

    use kithara::{
        events::{
            AdvanceReason, Event, EventBus, FileError, FileEvent, HlsError, HlsEvent, ItemRole,
            QueueEvent, QueueRepeatMode, SlotId, TrackId, TrackRef, TrackStatus,
        },
        platform::sync::{Arc, Mutex},
        play::{PlayWorkerConfig, PlayerConfig, PlayerImpl},
        queue::{QueueConfig, test_utils::QueueProbe},
    };

    use super::*;
    use crate::{
        observer::ItemObserver,
        pools::{self, FfiQueue, FfiWorker},
        types::{FfiItemConfig, FfiItemEvent},
    };

    type QueueEventCase = (QueueEvent, fn(&FfiPlayerEvent) -> bool);

    #[derive(Default)]
    struct CollectingPlayerObserver {
        events: Mutex<Vec<FfiPlayerEvent>>,
    }

    impl CollectingPlayerObserver {
        fn take_events(&self) -> Vec<FfiPlayerEvent> {
            std::mem::take(&mut *self.events.lock())
        }
    }

    impl PlayerObserver for CollectingPlayerObserver {
        fn on_event(&self, event: FfiPlayerEvent) {
            self.events.lock().push(event);
        }
    }

    #[derive(Default)]
    struct CollectingItemObserver {
        changed: Condvar,
        events: StdMutex<Vec<FfiItemEvent>>,
    }

    impl CollectingItemObserver {
        fn take_events(&self) -> Vec<FfiItemEvent> {
            let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *events)
        }

        fn wait_for_events(&self, count: usize) {
            let (events, _) = self
                .changed
                .wait_timeout_while(
                    self.events.lock().unwrap_or_else(PoisonError::into_inner),
                    Duration::from_secs(2),
                    |events| events.len() < count,
                )
                .unwrap_or_else(PoisonError::into_inner);
            assert!(
                events.len() >= count,
                "timed out waiting for {count} item events, received {events:?}"
            );
        }
    }

    impl ItemObserver for CollectingItemObserver {
        fn on_event(&self, event: FfiItemEvent) {
            self.events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
            self.changed.notify_all();
        }
    }

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

    fn register_observed_item(
        items: &Arc<Mutex<ItemRegistry>>,
    ) -> (Arc<AudioPlayerItem>, Arc<CollectingItemObserver>) {
        let item = AudioPlayerItem::new(item_config());
        let observer = Arc::new(CollectingItemObserver::default());
        let item_observer: Arc<dyn ItemObserver> = observer.clone();
        item.set_observer(item_observer);
        items.lock().insert(item.track_id(), item.clone());
        (item, observer)
    }

    fn assert_protocol_failure_is_not_duplicated(event: Event, expected_error: &str) {
        let root = EventBus::new(16);
        let scoped = root.scoped();
        let item = AudioPlayerItem::new(item_config());
        *item.inserted.lock() = true;
        item.state.lock().resolve_duration(42.0);

        let item_observer_impl = Arc::new(CollectingItemObserver::default());
        let item_observer: Arc<dyn ItemObserver> = item_observer_impl.clone();
        *item.bus.lock() = Some(scoped.clone());
        item.set_observer(item_observer);

        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        items.lock().insert(item.track_id(), item.clone());
        let player_observer_impl = Arc::new(CollectingPlayerObserver::default());
        let player_observer: Arc<dyn PlayerObserver> = player_observer_impl.clone();

        scoped.publish(event);
        item_observer_impl.wait_for_events(2);
        EventBridge::dispatch_queue_event(
            &player_observer,
            &items,
            &QueueEvent::TrackStatusChanged {
                id: item.track_id(),
                status: TrackStatus::Failed("queue load failed".to_string()),
            },
        );

        let item_events = item_observer_impl.take_events();
        assert_eq!(
            item_events.len(),
            2,
            "protocol failure must emit one item status/error pair, received {item_events:?}"
        );
        assert!(matches!(
            item_events.as_slice(),
            [
                FfiItemEvent::StatusChanged {
                    status: FfiItemStatus::Failed,
                },
                FfiItemEvent::Error { error },
            ] if error == expected_error
        ));
        assert_eq!(
            item.duration_sec(),
            0.0,
            "protocol failure must mark item failed"
        );
        assert!(matches!(
            player_observer_impl.take_events().as_slice(),
            [FfiPlayerEvent::TrackStatusChanged {
                item_id,
                status: FfiTrackStatus::Failed { reason },
            }] if *item_id == item.track_id() && reason == "queue load failed"
        ));
    }

    #[kithara::test]
    fn event_bridge_is_send() {
        assert_send::<EventBridge>();
    }

    #[kithara::test]
    fn hls_protocol_failure_is_not_duplicated_by_queue_status() {
        assert_protocol_failure_is_not_duplicated(
            Event::Hls(HlsEvent::Error {
                error: HlsError::Playlist("boom".into()),
            }),
            "item failed: playlist: boom",
        );
    }

    #[kithara::test]
    fn file_protocol_failure_is_not_duplicated_by_queue_status() {
        assert_protocol_failure_is_not_duplicated(
            Event::File(FileEvent::Error {
                error: FileError::Io("boom".into()),
            }),
            "item failed: io: boom",
        );
    }

    /// Only HLS, File and Downloader errors convert to an item error, so a
    /// failure settled without one — a decode, DRM or storage refusal — has no
    /// protocol bridge behind it. The queue is then the only source the item
    /// has, and silence here is the item never learning it failed.
    #[kithara::test]
    fn queue_failure_without_a_protocol_event_still_reaches_the_item() {
        let item = AudioPlayerItem::new(item_config());
        *item.inserted.lock() = true;
        let item_observer_impl = Arc::new(CollectingItemObserver::default());
        let item_observer: Arc<dyn ItemObserver> = item_observer_impl.clone();
        item.set_observer(item_observer);
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        items.lock().insert(item.track_id(), item.clone());
        let player_observer: Arc<dyn PlayerObserver> =
            Arc::new(CollectingPlayerObserver::default());

        EventBridge::dispatch_queue_event(
            &player_observer,
            &items,
            &QueueEvent::TrackStatusChanged {
                id: item.track_id(),
                status: TrackStatus::Failed("decoder refused the stream".to_string()),
            },
        );

        assert!(matches!(
            item_observer_impl.take_events().as_slice(),
            [
                FfiItemEvent::StatusChanged {
                    status: FfiItemStatus::Failed,
                },
                FfiItemEvent::Error { error },
            ] if error == "decoder refused the stream"
        ));
    }

    #[kithara::test]
    fn terminal_events_route_to_their_exact_item_when_sources_repeat() {
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        let (delayed, delayed_observer) = register_observed_item(&items);
        let (current, current_observer) = register_observed_item(&items);
        assert_ne!(delayed.track_id(), current.track_id());

        let last_current = Mutex::new(Some(current.track_id()));
        let shared_src: Arc<str> = Arc::from("https://example.com/quiet-intro.flac");
        EventBridge::route_player_event_to_item(
            &items,
            &last_current,
            &PlayerEvent::ItemDidPlayToEnd {
                item: ItemRole::Background(TrackRef::new(
                    delayed.track_id(),
                    SlotId::new(1),
                    Arc::clone(&shared_src),
                )),
            },
        );

        assert!(matches!(
            delayed_observer.take_events().as_slice(),
            [FfiItemEvent::DidReachEnd]
        ));
        assert!(
            current_observer.take_events().is_empty(),
            "a delayed background EOF must not be delivered to the current item"
        );

        EventBridge::route_player_event_to_item(
            &items,
            &last_current,
            &PlayerEvent::ItemDidFail {
                item: ItemRole::Outgoing(TrackRef::new(
                    delayed.track_id(),
                    SlotId::new(0),
                    shared_src,
                )),
            },
        );

        assert!(matches!(
            delayed_observer.take_events().as_slice(),
            [FfiItemEvent::DidFail]
        ));
        assert!(
            current_observer.take_events().is_empty(),
            "an outgoing failure must not be delivered to another item with the same source"
        );
    }

    #[kithara::test]
    fn event_without_item_identity_routes_to_last_current() {
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        let (_previous, previous_observer) = register_observed_item(&items);
        let (current, current_observer) = register_observed_item(&items);
        let last_current = Mutex::new(Some(current.track_id()));

        EventBridge::route_player_event_to_item(
            &items,
            &last_current,
            &PlayerEvent::TimeControlStatusChanged {
                status: TimeControlStatus::WaitingToPlay,
                reason: None,
            },
        );

        assert!(previous_observer.take_events().is_empty());
        assert!(matches!(
            current_observer.take_events().as_slice(),
            [FfiItemEvent::DidStall]
        ));
    }

    /// The queue can settle a track before the protocol bridge delivers its
    /// own error, so the pair must be owned by whoever arrives first rather
    /// than by a fixed source.
    #[kithara::test]
    fn a_second_queue_failure_does_not_repeat_the_pair() {
        let item = AudioPlayerItem::new(item_config());
        *item.inserted.lock() = true;
        let item_observer_impl = Arc::new(CollectingItemObserver::default());
        let item_observer: Arc<dyn ItemObserver> = item_observer_impl.clone();
        item.set_observer(item_observer);
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        items.lock().insert(item.track_id(), item.clone());
        let player_observer: Arc<dyn PlayerObserver> =
            Arc::new(CollectingPlayerObserver::default());
        let failed = QueueEvent::TrackStatusChanged {
            id: item.track_id(),
            status: TrackStatus::Failed("decoder refused the stream".to_string()),
        };

        EventBridge::dispatch_queue_event(&player_observer, &items, &failed);
        let _first_pair = item_observer_impl.take_events();
        EventBridge::dispatch_queue_event(&player_observer, &items, &failed);

        assert!(
            item_observer_impl.take_events().is_empty(),
            "a settled item must report its terminal pair once"
        );
    }

    /// The available window covers the playhead, so loaded ranges built from
    /// it keep the item playable — unlike the old byte-ratio telemetry that
    /// under-reported a VBR-FLAC quiet intro (~0.66s decoded byte-ratio at a
    /// 0.917s playhead) and made the host pause into a buffering deadlock.
    #[kithara::test]
    fn loaded_ranges_cover_playhead() {
        let item = AudioPlayerItem::new(item_config());
        let ranges = EventBridge::loaded_ranges(4.0);
        assert!(item.is_playable(0.917, ranges));
    }

    /// A fully cached track reports its whole span, not just what a decoder
    /// running a few seconds ahead of the playhead has produced.
    #[kithara::test]
    fn loaded_ranges_cover_the_cached_span() {
        let ranges = EventBridge::loaded_ranges(120.0);
        assert_eq!(ranges.len(), 1);
        assert!((ranges[0].duration_seconds - 120.0).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn loaded_ranges_empty_when_nothing_is_available() {
        assert!(EventBridge::loaded_ranges(0.0).is_empty());
    }

    #[kithara::test]
    fn current_track_advance_emits_advanced_only() {
        let observer_impl = Arc::new(CollectingPlayerObserver::default());
        let observer: Arc<dyn PlayerObserver> = observer_impl.clone();
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        let item_id = TrackId::from(7_u64);

        EventBridge::dispatch_queue_event(
            &observer,
            &items,
            &QueueEvent::CurrentTrackAdvance {
                id: Some(item_id),
                reason: AdvanceReason::UserNext,
            },
        );

        let events = observer_impl.take_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            FfiPlayerEvent::CurrentItemAdvanced {
                item_id: Some(id),
                reason: FfiAdvanceReason::UserNext,
            } if *id == item_id
        ));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, FfiPlayerEvent::CurrentItemChanged { .. }))
        );
    }

    #[kithara::test]
    fn repeat_mode_changed_maps_to_ffi_repeat_mode() {
        let observer_impl = Arc::new(CollectingPlayerObserver::default());
        let observer: Arc<dyn PlayerObserver> = observer_impl.clone();
        let items = Arc::new(Mutex::new(ItemRegistry::default()));

        EventBridge::dispatch_queue_event(
            &observer,
            &items,
            &QueueEvent::RepeatModeChanged {
                mode: QueueRepeatMode::All,
            },
        );

        assert!(matches!(
            observer_impl.take_events().as_slice(),
            [FfiPlayerEvent::RepeatModeChanged {
                mode: FfiRepeatMode::All,
            }]
        ));
    }

    #[kithara::test]
    fn track_load_failed_passes_reason_and_auto_skipped() {
        let observer_impl = Arc::new(CollectingPlayerObserver::default());
        let observer: Arc<dyn PlayerObserver> = observer_impl.clone();
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        let item_id = TrackId::from(11_u64);

        EventBridge::dispatch_queue_event(
            &observer,
            &items,
            &QueueEvent::TrackLoadFailed {
                id: item_id,
                reason: "network timeout".to_string(),
                auto_skipped: true,
            },
        );

        assert!(matches!(
            observer_impl.take_events().as_slice(),
            [FfiPlayerEvent::TrackLoadFailed {
                item_id: id,
                reason,
                auto_skipped: true,
            }] if *id == item_id && reason == "network timeout"
        ));
    }

    #[kithara::test]
    fn queue_dispatch_forwards_every_remaining_host_event() {
        let item_id = TrackId::from(17_u64);
        let cases: [QueueEventCase; 7] = [
            (
                QueueEvent::TrackAdded {
                    id: item_id,
                    index: 2,
                },
                |event| matches!(event, FfiPlayerEvent::TrackAdded { item_id: id, index: 2 } if *id == TrackId::from(17_u64)),
            ),
            (
                QueueEvent::TrackRemoved { id: item_id },
                |event| matches!(event, FfiPlayerEvent::TrackRemoved { item_id: id } if *id == TrackId::from(17_u64)),
            ),
            (
                QueueEvent::CurrentTrackChanged { id: Some(item_id) },
                |event| matches!(event, FfiPlayerEvent::CurrentItemChanged { item_id: Some(id) } if *id == TrackId::from(17_u64)),
            ),
            (QueueEvent::QueueEnded, |event| {
                matches!(event, FfiPlayerEvent::QueueEnded)
            }),
            (
                QueueEvent::CrossfadeStarted {
                    duration_seconds: 3.5,
                },
                |event| {
                    matches!(
                        event,
                        FfiPlayerEvent::CrossfadeStarted {
                            duration_seconds: 3.5
                        }
                    )
                },
            ),
            (
                QueueEvent::CrossfadeDurationChanged { seconds: 4.0 },
                |event| {
                    matches!(
                        event,
                        FfiPlayerEvent::CrossfadeDurationChanged { seconds: 4.0 }
                    )
                },
            ),
            (
                QueueEvent::NextTrackReady {
                    id: item_id,
                    index: 3,
                },
                |event| matches!(event, FfiPlayerEvent::NextTrackReady { item_id: id, index: 3 } if *id == TrackId::from(17_u64)),
            ),
        ];
        let observer_impl = Arc::new(CollectingPlayerObserver::default());
        let observer: Arc<dyn PlayerObserver> = observer_impl.clone();
        let items = Arc::new(Mutex::new(ItemRegistry::default()));

        for (source, preserves_contract) in &cases {
            EventBridge::dispatch_queue_event(&observer, &items, source);
            let events = observer_impl.take_events();
            let [event] = events.as_slice() else {
                panic!("expected one forwarded event for {source:?}, received {events:?}");
            };
            assert!(preserves_contract(event), "unexpected event: {event:?}");
        }
    }

    async fn wait_for_status(
        events: &mut EventReceiver,
        id: TrackId,
        status: TrackStatus,
        timeout_ms: u64,
    ) -> bool {
        let wait = async {
            while let Ok(Envelope { event, .. }) = events.recv().await {
                if matches!(
                    event,
                    Event::Queue(QueueEvent::TrackStatusChanged { id: seen, status: ref seen_status })
                        if seen == id && *seen_status == status
                ) {
                    return true;
                }
            }
            false
        };
        kithara::platform::time::timeout(Duration::from_millis(timeout_ms), wait)
            .await
            .unwrap_or(false)
    }

    /// The polling thread drives `Queue::tick`, and a natural EOF on a
    /// repeat-one track makes that tick respawn the consumed track's load
    /// — async work that panics without an ambient runtime. The thread is
    /// a plain OS thread, so it only has one if it enters `FFI_RUNTIME`
    /// itself.
    #[kithara::test(tokio)]
    async fn polling_thread_reloads_a_consumed_track_after_eof() {
        let worker = FfiWorker::new(
            PlayWorkerConfig::builder(pools::build().expect("valid FFI pool policy")).build(),
        );
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(crate::native::session::requested_sample_rate())
                .worker(worker)
                .build(),
        );
        let queue = FfiQueue::new(QueueConfig::builder().player(player).build());
        let owner = crate::native::session::insert(queue)
            .expect("INVARIANT: the FFI test Host accepts its allocated Queue");
        let queue = owner.control().clone();
        let id = queue.register_for_test();
        queue.mark_played_for_test(id);
        queue.set_repeat(kithara::queue::RepeatMode::One);
        queue.set_rate(1.0);

        let mut events = queue.subscribe();
        queue
            .bus()
            .publish(Event::Player(PlayerEvent::ItemDidPlayToEnd {
                item: ItemRole::Leading(TrackRef::new(
                    id,
                    SlotId::new(0),
                    Arc::from(format!("test://memory/{}", id.as_u64())),
                )),
            }));

        let cancel = CancelToken::root();
        let observer: Arc<dyn PlayerObserver> = Arc::new(CollectingPlayerObserver::default());
        let thread = EventBridge::spawn_time_thread(
            queue.clone(),
            observer,
            Arc::new(Mutex::new(ItemRegistry::default())),
            Arc::new(Mutex::new(None)),
            cancel.clone(),
        );

        let reload_started = wait_for_status(&mut events, id, TrackStatus::Pending, 2000).await;
        cancel.cancel();
        let joined = thread.join();
        crate::native::session::remove(&owner)
            .expect("INVARIANT: the FFI test Queue detaches from its Host");

        assert!(
            reload_started,
            "tick after EOF must restart the consumed repeat-one track"
        );
        assert!(
            joined.is_ok(),
            "polling thread must survive the reload it starts"
        );
    }
}
