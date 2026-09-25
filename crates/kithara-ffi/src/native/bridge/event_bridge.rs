use kithara::{
    events::{Envelope, EventReceiver, TrackId},
    platform::{
        CancelToken,
        sync::{Arc, Mutex},
        thread::{JoinHandle, sleep, spawn},
        time::Duration,
        tokio,
        tokio::sync::broadcast,
    },
    play::{PlayerEvent, TimeControlStatus},
    queue::QueueEvent,
};

use crate::{
    core::event_set::QueueBusEvent,
    observer::PlayerObserver,
    pools::FfiQueueControl,
    registry::ItemRegistry,
    types::{
        FfiActionAtItemEnd, FfiAdvanceReason, FfiCrossfadeSettings, FfiItemEvent, FfiPlaybackOrder,
        FfiPlayerEvent, FfiRepeatMode, FfiTimeRange, FfiTrackStatus,
    },
};

pub(crate) struct EventBridge {
    cancel: CancelToken,
    time_thread: Option<JoinHandle<()>>,
}

impl EventBridge {
    /// Failure reason recorded when the player reports an item failure
    /// without one.
    const ITEM_DID_FAIL: &'static str = "item did fail";

    /// Polling interval for time/duration updates (~10 Hz).
    const TIME_POLL_INTERVAL_MS: u64 = 100;

    /// Threshold for suppressing redundant time/duration updates (seconds).
    const TIME_UPDATE_THRESHOLD: f64 = 0.01;

    fn dispatch(
        observer: &Arc<dyn PlayerObserver>,
        items: &Arc<Mutex<ItemRegistry>>,
        last_current: &Mutex<Option<TrackId>>,
        event: &QueueBusEvent,
    ) {
        if let QueueBusEvent::Player(pe) = event {
            if let PlayerEvent::CurrentItemChanged { item } = pe {
                *last_current.lock() = *item;
            }
            Self::route_player_event_to_item(items, last_current, pe);
            let Some(ffi_event) = FfiPlayerEvent::try_from(pe).ok() else {
                return;
            };
            observer.on_event(ffi_event);
            return;
        }
        if let QueueBusEvent::Queue(qe) = event {
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
                let status = FfiTrackStatus::from(status.clone());
                item.apply_track_status(&status);
                observer.on_event(FfiPlayerEvent::TrackStatusChanged {
                    status,
                    item_id: *id,
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
            QueueEvent::CrossfadeStarted { settings } => {
                observer.on_event(FfiPlayerEvent::CrossfadeStarted {
                    settings: FfiCrossfadeSettings::from(*settings),
                });
            }
            QueueEvent::CrossfadeSettingsChanged { settings } => {
                observer.on_event(FfiPlayerEvent::CrossfadeSettingsChanged {
                    settings: FfiCrossfadeSettings::from(*settings),
                });
            }
            QueueEvent::PlaybackOrderChanged { order } => {
                observer.on_event(FfiPlayerEvent::PlaybackOrderChanged {
                    order: FfiPlaybackOrder::from(*order),
                });
            }
            QueueEvent::ActionAtItemEndChanged { action } => {
                observer.on_event(FfiPlayerEvent::ActionAtItemEndChanged {
                    action: FfiActionAtItemEnd::from(*action),
                });
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
        *last = Some(available);
        item.deliver(FfiItemEvent::LoadedRangesChanged {
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
            PlayerEvent::ItemDidPlayToEnd { item } | PlayerEvent::ItemDidFail { item, .. } => {
                Some(item.id())
            }
            PlayerEvent::TimeControlStatusChanged {
                status: TimeControlStatus::WaitingToPlay,
                ..
            } => *last_current.lock(),
            PlayerEvent::TimeControlStatusChanged {
                status: TimeControlStatus::Paused | TimeControlStatus::Playing,
                ..
            }
            | PlayerEvent::StatusChanged { .. }
            | PlayerEvent::RateChanged { .. }
            | PlayerEvent::PlaybackStarted { .. }
            | PlayerEvent::VolumeChanged { .. }
            | PlayerEvent::MuteChanged { .. }
            | PlayerEvent::CurrentItemChanged { .. }
            | PlayerEvent::PrerollCompleted { .. }
            | PlayerEvent::PrefetchRequested
            | PlayerEvent::HandoverRequested { .. } => return,
        };
        let Some(track_id) = target else { return };
        let Some(item) = items.lock().get(&track_id).cloned() else {
            return;
        };
        let ffi_event = match event {
            PlayerEvent::ItemDidPlayToEnd { .. } => FfiItemEvent::DidReachEnd,
            PlayerEvent::ItemDidFail { .. } => {
                item.settle_failed(Self::ITEM_DID_FAIL);
                FfiItemEvent::DidFail
            }
            PlayerEvent::TimeControlStatusChanged { .. } => FfiItemEvent::DidStall,
            PlayerEvent::StatusChanged { .. }
            | PlayerEvent::RateChanged { .. }
            | PlayerEvent::PlaybackStarted { .. }
            | PlayerEvent::VolumeChanged { .. }
            | PlayerEvent::MuteChanged { .. }
            | PlayerEvent::CurrentItemChanged { .. }
            | PlayerEvent::PrerollCompleted { .. }
            | PlayerEvent::PrefetchRequested
            | PlayerEvent::HandoverRequested { .. } => return,
        };
        item.deliver(ffi_event);
    }

    /// Spawn background tasks that translate queue/player events into
    /// observer callbacks. Returns a bridge handle; dropping it cancels
    /// the tasks.
    pub(crate) fn spawn(
        rx: EventReceiver<QueueBusEvent>,
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
        mut rx: EventReceiver<QueueBusEvent>,
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
        audio::DecodeErrorKind,
        events::{EventBus, SlotId, TrackId},
        platform::{
            sync::{Arc, Mutex},
            tokio::task::spawn_blocking,
        },
        play::{ItemRole, PlayWorkerConfig, PlaybackFault, PlayerConfig, PlayerImpl, TrackRef},
        queue::{AdvanceReason, QueueConfig, QueueEvent, QueueRepeatMode, TrackStatus, Transition},
    };
    use kithara_file::{FileError, FileEvent};
    use kithara_hls::{HlsEvent, HlsFailure};
    use kithara_test_fixtures::assets;

    use super::*;
    use crate::{
        core::event_set::ItemBusEvent,
        item::AudioPlayerItem,
        observer::ItemObserver,
        pools,
        pools::{FfiQueue, FfiWorker},
        types::{FfiItemConfig, FfiItemEvent, FfiItemStatus},
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
        FfiItemConfig::for_test("https://example.com/quiet-intro.flac")
    }

    fn register_observed_item(
        items: &Arc<Mutex<ItemRegistry>>,
    ) -> (Arc<AudioPlayerItem>, Arc<CollectingItemObserver>) {
        let item = AudioPlayerItem::new(item_config());
        let observer = Arc::new(CollectingItemObserver::default());
        let item_observer: Arc<dyn ItemObserver> = observer.clone();
        item.add_observer(item_observer);
        items.lock().insert(item.track_id(), item.clone());
        (item, observer)
    }

    #[kithara::test]
    fn state_outlives_the_observer_that_saw_the_events() {
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        let (item, _observer) = register_observed_item(&items);
        let player_observer: Arc<dyn PlayerObserver> =
            Arc::new(CollectingPlayerObserver::default());

        EventBridge::dispatch_queue_event(
            &player_observer,
            &items,
            &QueueEvent::TrackStatusChanged {
                id: item.track_id(),
                status: TrackStatus::Failed("storage refused".to_string()),
            },
        );

        let late = Arc::new(CollectingItemObserver::default());
        item.add_observer(late.clone() as Arc<dyn ItemObserver>);

        let state = item.state();
        assert_eq!(state.status, FfiItemStatus::Failed);
        assert_eq!(state.error.as_deref(), Some("storage refused"));
        assert!(late.take_events().is_empty());
    }

    fn assert_protocol_failure_is_not_duplicated(event: ItemBusEvent, expected_error: &str) {
        let root = EventBus::new(16);
        let scoped = root.scoped();
        let item = AudioPlayerItem::new(item_config());
        *item.inserted.lock() = true;
        item.deliver(FfiItemEvent::DurationChanged { seconds: 42.0 });

        let item_observer_impl = Arc::new(CollectingItemObserver::default());
        let item_observer: Arc<dyn ItemObserver> = item_observer_impl.clone();
        *item.bus.lock() = Some(scoped.clone());
        item.add_observer(item_observer);
        item.restart_bridge();

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
            ItemBusEvent::Hls(HlsEvent::Error {
                error: HlsFailure::Playlist("boom".into()),
            }),
            "item failed: playlist: boom",
        );
    }

    #[kithara::test]
    fn file_protocol_failure_is_not_duplicated_by_queue_status() {
        assert_protocol_failure_is_not_duplicated(
            ItemBusEvent::File(FileEvent::Error {
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
        item.add_observer(item_observer);
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
                fault: PlaybackFault::Decode(DecodeErrorKind::InvalidData),
            },
        );

        assert!(matches!(
            delayed_observer.take_events().as_slice(),
            [
                FfiItemEvent::StatusChanged {
                    status: FfiItemStatus::Failed
                },
                FfiItemEvent::Error { .. },
                FfiItemEvent::DidFail
            ]
        ));
        assert_eq!(
            delayed.state().error.as_deref(),
            Some(EventBridge::ITEM_DID_FAIL)
        );
        assert!(
            current_observer.take_events().is_empty(),
            "an outgoing failure must not be delivered to another item with the same source"
        );
    }

    #[kithara::test]
    fn a_current_item_event_updates_the_last_current_identity() {
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        let observer: Arc<dyn PlayerObserver> = Arc::new(CollectingPlayerObserver::default());
        let last_current = Mutex::new(None);
        let id = TrackId::from(11_u64);

        EventBridge::dispatch(
            &observer,
            &items,
            &last_current,
            &QueueBusEvent::Player(PlayerEvent::CurrentItemChanged { item: Some(id) }),
        );
        assert_eq!(*last_current.lock(), Some(id));
        EventBridge::dispatch(
            &observer,
            &items,
            &last_current,
            &QueueBusEvent::Queue(QueueEvent::CurrentTrackChanged { id: None }),
        );
        assert_eq!(*last_current.lock(), Some(id));
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
        item.add_observer(item_observer);
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
                    settings: kithara::play::CrossfadeSettings {
                        duration: 3.5,
                        ..Default::default()
                    },
                },
                |event| {
                    matches!(
                        event,
                        FfiPlayerEvent::CrossfadeStarted {
                            settings: FfiCrossfadeSettings { duration: 3.5, .. }
                        }
                    )
                },
            ),
            (
                QueueEvent::CrossfadeSettingsChanged {
                    settings: kithara::play::CrossfadeSettings {
                        duration: 4.0,
                        ..Default::default()
                    },
                },
                |event| {
                    matches!(
                        event,
                        FfiPlayerEvent::CrossfadeSettingsChanged {
                            settings: FfiCrossfadeSettings { duration: 4.0, .. }
                        }
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

    /// What an event wait actually observed, so a failure names the
    /// outcome instead of reporting a bare boolean.
    enum WaitOutcome {
        /// The awaited event arrived.
        Observed { lagged: u64, seen: usize },
        /// The bus closed before the awaited event arrived.
        Closed { lagged: u64, seen: usize },
        /// The budget expired before the awaited event arrived.
        TimedOut {
            budget_ms: u64,
            lagged: u64,
            seen: usize,
        },
    }

    impl WaitOutcome {
        fn observed(&self) -> bool {
            matches!(self, Self::Observed { .. })
        }
    }

    impl std::fmt::Display for WaitOutcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Observed { lagged, seen } => {
                    write!(f, "observed after {seen} event(s), {lagged} dropped")
                }
                Self::Closed { lagged, seen } => {
                    write!(f, "bus closed after {seen} event(s), {lagged} dropped")
                }
                Self::TimedOut {
                    budget_ms,
                    lagged,
                    seen,
                } => write!(
                    f,
                    "no event within {budget_ms}ms, after {seen} event(s), {lagged} dropped"
                ),
            }
        }
    }

    /// Waits for an event the bus may deliver behind a dropped burst.
    ///
    /// `Lagged` reports that the bus dropped the oldest envelopes and kept
    /// delivering, so the wait continues exactly as the bridge's own event
    /// task does; only `Closed` and the budget end it.
    #[kithara_test_utils::kithara::hang_watchdog]
    async fn wait_for_event(
        events: &mut EventReceiver<QueueBusEvent>,
        timeout_ms: u64,
        is_awaited: impl Fn(&QueueBusEvent) -> bool,
    ) -> WaitOutcome {
        let mut lagged = 0_u64;
        let mut seen = 0_usize;
        let ended = {
            let lagged = &mut lagged;
            let seen = &mut seen;
            let wait = async move {
                loop {
                    match events.recv().await {
                        Ok(Envelope { event, .. }) => {
                            *seen += 1;
                            if is_awaited(&event) {
                                return true;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(dropped)) => *lagged += dropped,
                        Err(broadcast::error::RecvError::Closed) => return false,
                    }
                }
            };
            kithara::platform::time::timeout(Duration::from_millis(timeout_ms), wait).await
        };
        match ended {
            Ok(true) => WaitOutcome::Observed { lagged, seen },
            Ok(false) => WaitOutcome::Closed { lagged, seen },
            Err(_) => WaitOutcome::TimedOut {
                lagged,
                seen,
                budget_ms: timeout_ms,
            },
        }
    }

    async fn wait_for_status(
        events: &mut EventReceiver<QueueBusEvent>,
        id: TrackId,
        status: TrackStatus,
        timeout_ms: u64,
    ) -> WaitOutcome {
        wait_for_event(events, timeout_ms, |event| {
            matches!(
                event,
                QueueBusEvent::Queue(QueueEvent::TrackStatusChanged { id: seen, status: seen_status })
                    if *seen == id && *seen_status == status
            )
        })
        .await
    }

    async fn wait_for_eof_advance(
        events: &mut EventReceiver<QueueBusEvent>,
        id: TrackId,
        timeout_ms: u64,
    ) -> WaitOutcome {
        wait_for_event(events, timeout_ms, |event| {
            matches!(
                event,
                QueueBusEvent::Queue(QueueEvent::CurrentTrackAdvance {
                    reason: AdvanceReason::NaturalEof,
                    id: Some(seen),
                }) if *seen == id
            )
        })
        .await
    }

    /// The polling thread drives `Queue::tick`, including repeat-one replay.
    #[kithara::test(tokio, flash(false))]
    async fn polling_thread_replays_a_consumed_track_after_eof() {
        crate::native::session::initialize_test_host();
        let worker = FfiWorker::new(
            PlayWorkerConfig::builder(pools::build().expect("valid FFI pool policy")).build(),
        );
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(
                    crate::native::session::requested_sample_rate()
                        .expect("initialized test Host has a sample rate"),
                )
                .worker(worker)
                .build(),
        );
        let queue = FfiQueue::new(QueueConfig::builder().player(player).build());
        // The FFI surface calls the session from the caller's thread, never
        // from a runtime worker.
        let owner = spawn_blocking(move || crate::native::session::insert(queue))
            .await
            .expect("insert task completes")
            .expect("INVARIANT: the FFI test Host accepts its allocated Queue");
        let queue = owner.control().clone();
        queue.set_repeat(kithara::queue::RepeatMode::One);
        queue.set_rate(1.0);
        let mut events = queue.subscribe();
        let track = assets::sine_wav_a440_100_frames()
            .path()
            .expect("the short decoder WAV lives on disk");
        let id = queue
            .append(track.to_string_lossy().into_owned())
            .expect("open queue accepts a local track");
        let loaded = wait_for_status(&mut events, id, TrackStatus::Loaded, 2000).await;
        assert!(
            loaded.observed(),
            "real local track must load before playback; wait: {loaded}"
        );

        let cancel = CancelToken::root();
        let observer: Arc<dyn PlayerObserver> = Arc::new(CollectingPlayerObserver::default());
        let thread = EventBridge::spawn_time_thread(
            queue.clone(),
            observer,
            Arc::new(Mutex::new(ItemRegistry::default())),
            Arc::new(Mutex::new(None)),
            cancel.clone(),
        );

        let selecting = queue.clone();
        spawn_blocking(move || selecting.select(id, Transition::None))
            .await
            .expect("select task completes")
            .expect("loaded track starts through the real queue lifecycle");

        let replay = wait_for_eof_advance(&mut events, id, 2000).await;
        let status = queue.track(id).map(|entry| entry.status);
        cancel.cancel();
        let (joined, owner) = spawn_blocking(move || {
            let joined = thread.join();
            crate::native::session::remove(&owner)
                .expect("INVARIANT: the FFI test Queue detaches from its Host");
            (joined, owner)
        })
        .await
        .expect("teardown task completes");
        drop(owner);

        assert!(
            replay.observed(),
            "tick after EOF must replay the consumed repeat-one track; wait: {replay}, status: {status:?}"
        );
        assert!(
            joined.is_ok(),
            "polling thread must survive the reload it starts"
        );
    }

    /// A burst that outruns the bus drops the oldest envelopes and keeps
    /// delivering, so a wait must survive the gap instead of reading it as
    /// the awaited event never arriving.
    #[kithara::test(tokio)]
    async fn a_wait_survives_a_lagged_bus_and_still_observes_the_next_event() {
        const CAPACITY: usize = 4;

        let bus = EventBus::new(CAPACITY);
        let mut events = bus.subscribe::<QueueBusEvent>();
        let id = TrackId::from(23_u64);

        for _ in 0..=CAPACITY {
            bus.publish(QueueEvent::TrackStatusChanged {
                id,
                status: TrackStatus::Loading,
            });
        }
        bus.publish(QueueEvent::CurrentTrackAdvance {
            reason: AdvanceReason::NaturalEof,
            id: Some(id),
        });

        let outcome = wait_for_eof_advance(&mut events, id, 2000).await;
        assert!(
            outcome.observed(),
            "a dropped burst must not end the wait; wait: {outcome}"
        );
        assert!(
            matches!(&outcome, WaitOutcome::Observed { lagged, .. } if *lagged > 0),
            "the burst must outrun the bus, else the wait never met a gap; wait: {outcome}"
        );
    }
}
