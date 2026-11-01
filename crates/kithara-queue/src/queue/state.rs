use kithara_assets::{AssetStore, StorageBackend};
use kithara_bufpool::HasPool;
use kithara_command::mailbox;
use kithara_events::{EventBus, TrackId};
use kithara_platform::{CancelScope, CancelToken, tokio::runtime::Handle as RuntimeHandle};
use kithara_play::{
    PlayError, Player, PlayerEvent, PlayerFactory, TrackFactory, TrackStatus as PlayingStatus,
};
use kithara_signal::{FrameCount, SessionFrame};

use super::{
    command::{QueueMailbox, QueuePostbox},
    slots::{Role, Slots},
    types::{PendingHandover, SeekPosition, Target},
    view::{DeckObservation, QueueView},
};
use crate::{QueueConfig, QueueEvent, loader::Loader, navigation::NavigationState, track::Tracks};

/// Cloneable command capability and the queue's published state.
/// Commands answer after the owner accepts and sends them; executor effects
/// enter the published state on their receipts.
#[derive_where::derive_where(Clone)]
pub struct QueueControl<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(super) postbox: QueuePostbox<S>,
    pub(super) view: QueueView<S>,
    pub(super) bus: EventBus,
}

/// A deck whose track list, navigation and active tracks have one owner.
pub struct Queue<S, F = PlayerFactory>
where
    S: HasPool<u8> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(super) config: QueueConfig<S, F>,
    pub(super) tracks: Tracks<S>,
    pub(super) navigation: NavigationState,
    pub(super) current: Option<TrackId>,
    pub(super) held_position: Option<SeekPosition>,
    pub(super) target: Option<Target>,
    pub(super) pending_handover: Option<PendingHandover>,
    pub(super) active: Slots<F::Track>,
    pub(super) postbox: QueuePostbox<S>,
    pub(super) mailbox: QueueMailbox<S>,
    pub(super) view: QueueView<S>,
    pub(super) bus: EventBus,
    pub(super) loader: Option<Loader<S>>,
    pub(super) shutdown: CancelToken,
    pub(super) events: Vec<QueueEvent>,
    pub(super) clock: Option<(SessionFrame, FrameCount)>,
    pub(super) deck: DeckObservation,
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    /// Builds the owner; its host checks registration before driving it.
    #[must_use]
    pub fn new(mut config: QueueConfig<S, F>) -> Self {
        let shutdown = CancelScope::new(config.cancel.clone()).token();
        if let Some(prep) = &mut config.prep {
            prep.cancel = Some(shutdown.clone());
        }
        let bus = config
            .prep
            .as_ref()
            .map_or_else(EventBus::default, |prep| prep.bus.clone());
        let (postbox, mailbox) = mailbox();
        let loader = config.prep.as_ref().map(|prep| {
            let store = config.store.clone().unwrap_or_else(|| {
                AssetStore::builder(prep.worker.pools().clone())
                    .backend(StorageBackend::default())
                    .cancel(shutdown.child())
                    .build()
            });
            Loader::new(
                prep.clone(),
                store,
                config.downloader.clone(),
                config
                    .runtime
                    .clone()
                    .or_else(|| RuntimeHandle::try_current().ok()),
                postbox.clone(),
            )
        });
        let mut navigation = NavigationState::new(config.max_history_size);
        navigation.set_playback_order(config.playback_order, &[], None);
        let tracks = Tracks::default();
        let deck = DeckObservation::new(config.mixer);
        let view = QueueView::new(
            &tracks,
            &navigation,
            config.settings,
            config.track,
            config.action_at_item_end,
            deck.clone(),
        );
        Self {
            active: Slots::new(config.mixer.slots().get()),
            config,
            tracks,
            navigation,
            current: None,
            held_position: None,
            target: None,
            pending_handover: None,
            postbox,
            mailbox,
            view,
            bus,
            loader,
            shutdown,
            events: Vec::new(),
            clock: None,
            deck,
        }
    }

    /// The handle reaches this owner's mailbox, never a track implementation.
    #[must_use]
    pub fn control(&self) -> QueueControl<S> {
        QueueControl {
            postbox: self.postbox.clone(),
            view: self.view.clone(),
            bus: self.bus.clone(),
        }
    }

    delegate::delegate! {
        to self.active {
            /// Every track the queue holds: on the deck, staged, and loaded in the background.
            #[call(tracks_mut)]
            pub fn tracks_mut(&mut self) -> impl Iterator<Item = &mut F::Track>;
            /// Every track the queue holds: on the deck, staged, and loaded in the background.
            #[call(tracks)]
            pub fn tracks_active(&self) -> impl Iterator<Item = &F::Track>;
        }
        to self.config {
            /// The factory a track decorator configures for subsequent loads.
            #[field(&mut factory)]
            pub fn factory_mut(&mut self) -> &mut F;
            /// The factory whose configuration subsequent tracks inherit.
            #[must_use]
            #[field(&factory)]
            pub fn factory(&self) -> &F;
        }
    }

    /// The sounding track, chosen only when its transition applies.
    #[must_use]
    pub fn current_track(&self) -> Option<&F::Track> {
        self.active
            .iter()
            .find(|active| {
                active.role == Role::Current
                    && !matches!(
                        active.track.snapshot().as_ref().status,
                        PlayingStatus::Ended { .. }
                            | PlayingStatus::Failed { .. }
                            | PlayingStatus::Released
                    )
            })
            .map(|active| &active.track)
    }

    pub(super) fn active_current_index(&self) -> Option<usize> {
        self.active.position(|active| active.role == Role::Current)
    }

    pub(super) fn earliest(&self) -> Result<SessionFrame, PlayError> {
        self.clock
            .map(|(now, delivery)| now + delivery)
            .ok_or(PlayError::Untimed)
    }

    pub(super) fn ensure_open(&self) -> Result<(), PlayError> {
        if self.shutdown.is_cancelled() {
            Err(PlayError::Closed)
        } else {
            Ok(())
        }
    }

    /// Records the tracks' earlier changes before the owner's next event.
    pub(super) fn announce(&mut self, event: QueueEvent) {
        self.events.extend(self.tracks.drain_events());
        self.events.push(event);
    }

    /// Publishes before announcing, so an event's reader sees its state.
    pub(super) fn publish(&mut self) {
        if let Some(SeekPosition::Pending { item, segment, .. }) = self.held_position {
            let active = self
                .active_current_index()
                .or_else(|| {
                    self.target.and_then(|target| {
                        self.active.position(|active| {
                            active.item == target.to && matches!(active.role, Role::Incoming)
                        })
                    })
                })
                .and_then(|index| self.active.get(index));
            if active.is_none_or(|active| active.item != item)
                || active.is_some_and(|active| {
                    segment.is_some_and(|segment| {
                        active
                            .track
                            .snapshot()
                            .as_ref()
                            .mark
                            .is_some_and(|mark| mark.lane.segment == segment)
                    })
                })
            {
                self.held_position = None;
            }
        }
        self.events.extend(self.tracks.drain_events());
        let snapshot = self.queue_snapshot();
        let previous = self.view.read();
        let rate = snapshot.applied_rate();
        let rate_changed = previous.applied_rate() != rate;
        let previous_mix = previous.deck.mix;
        let mix = snapshot.deck.mix;
        self.view.publish(snapshot);
        if rate_changed {
            self.bus.publish(PlayerEvent::RateChanged { rate });
        }
        if previous_mix.volume() != mix.volume() {
            self.bus.publish(PlayerEvent::VolumeChanged {
                volume: f32::from(mix.volume()),
            });
        }
        if previous_mix.muted() != mix.muted() {
            self.bus
                .publish(PlayerEvent::MuteChanged { muted: mix.muted() });
        }
        for event in self.events.drain(..) {
            if let QueueEvent::CurrentTrackChanged { id } = &event {
                self.bus
                    .publish(PlayerEvent::CurrentItemChanged { item: *id });
            }
            self.bus.publish(event);
        }
    }

    pub(super) fn track_ids(&self) -> Vec<TrackId> {
        self.tracks
            .records()
            .iter()
            .map(|record| record.id)
            .collect()
    }

    pub(super) fn navigation_cursor(&self) -> Option<TrackId> {
        self.target.map(|target| target.to).or(self.current)
    }

    pub(super) fn selectable_ids(&self) -> Vec<TrackId> {
        let cursor = self.navigation_cursor();
        self.tracks
            .records()
            .iter()
            .filter(|record| {
                self.current == Some(record.id)
                    || cursor == Some(record.id)
                    || !matches!(
                        record.status,
                        crate::TrackStatus::Cancelled | crate::TrackStatus::Failed(_)
                    )
            })
            .map(|record| record.id)
            .collect()
    }
}

impl<S, F> Drop for Queue<S, F>
where
    S: HasPool<u8> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.tracks.cancel_loads();
    }
}
#[cfg(test)]
pub(crate) mod tests {
    use std::{
        sync::Arc,
        task::{Wake, Waker},
    };

    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_command::{Seq, When};
    use kithara_config::Config;
    use kithara_events::{Envelope, EventReceiver, TrackId};
    use kithara_platform::{
        sync::mpsc,
        thread,
        time::{Duration, WallInstant, timeout},
        tokio::sync::mpsc::{UnboundedSender, unbounded_channel},
    };
    use kithara_play::{
        DeckMixerConfig, DeckPass, HostedDeck, Outbox, PlayError, PlayWorker, PlayWorkerConfig,
        Player, Position, ResourcePrep,
        mock::{self, DeckRig},
    };
    use kithara_signal::{FrameCount, SessionFrame};
    use kithara_test_utils::kithara;

    use super::{
        super::{QueueCommand, types::Placement},
        *,
    };
    use crate::{
        QueueError, QueueSettingsChange,
        event::{QueueEvent, QueueRepeatMode, TrackStatus},
        navigation::{ActionAtItemEnd, PlaybackOrder, RepeatMode},
        test_pools::{TestPools, pools},
    };

    /// No queue test ever streams bytes, so the store is here to be wired, not
    /// to hold anything. The default backend would map a file under the shared
    /// temp root, which every parallel test process also owns and which Miri
    /// cannot map at all.
    pub(in crate::queue) fn make_store() -> AssetStore<TestPools> {
        AssetStore::builder(pools())
            .backend(StorageBackend::Memory)
            .build()
    }

    /// A queue the mock session holds, seated the way a Host's insert seats it.
    /// A queue its Host has seated on a deck slot, with the mock that answers
    /// as the slot's audio thread.
    pub(in crate::queue) fn make_queue() -> (Queue<TestPools>, DeckRig<TestPools>) {
        let mut queue = Queue::new(queue_config());
        queue.clock = Some((SessionFrame::new(0), FrameCount::new(128)));
        queue.deck.mixer.sample_rate = mock::SAMPLE_RATE.get();
        let audio_thread = DeckRig::new(DeckMixerConfig::default()).expect("deck scope");
        (queue, audio_thread)
    }

    fn queue_config() -> QueueConfig<TestPools> {
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        QueueConfig::builder()
            .prep(ResourcePrep::builder().worker(worker).build())
            .store(make_store())
            .build()
    }

    pub(in crate::queue) fn with_outbox<Value>(
        queue: &mut Queue<TestPools>,
        rig: &mut DeckRig<TestPools>,
        run: impl FnOnce(&mut Queue<TestPools>, DeckPass<'_>, &mut Outbox<'_, TestPools>) -> Value,
    ) -> Value {
        let output = mock::output(None).get();
        let deck = queue.deck.mixer.clone();
        let pass = DeckPass {
            mix: queue.deck.mix,
            suspended: false,
            now: SessionFrame::new(0),
            delivery: FrameCount::new(128),
            output: &output,
            deck: &deck,
        };
        let mut scope = rig.ring.scope(rig.scope).expect("deck scope");
        let mut out = Outbox::new(&mut scope, &mut rig.dispatcher).in_pass(pass);
        run(queue, pass, &mut out)
    }

    pub(in crate::queue) fn apply(
        queue: &mut Queue<TestPools>,
        rig: &mut DeckRig<TestPools>,
        command: QueueCommand<TestPools>,
    ) -> Result<Option<Seq>, PlayError> {
        with_outbox(queue, rig, |queue, _, out| {
            Player::apply(queue, command, out)
        })
    }

    #[kithara::flash(io)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
    pub(in crate::queue) async fn wait_for_queue_event<F>(
        rx: &mut EventReceiver<QueueEvent>,
        mut matches: F,
        timeout_ms: u64,
    ) -> bool
    where
        F: FnMut(&QueueEvent) -> bool,
    {
        let deadline = WallInstant::now() + Duration::from_millis(timeout_ms);
        loop {
            hang_tick!("queue event predicate remains unsatisfied".to_owned());
            let remaining = deadline.saturating_duration_since(WallInstant::now());
            if remaining.is_zero() {
                return false;
            }
            match timeout(remaining.min(__hang_detector.remaining()), rx.recv()).await {
                Ok(Ok(Envelope { event: ev, .. })) if matches(&ev) => {
                    hang_reset!();
                    return true;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => return false,
            }
        }
    }

    #[kithara::test]
    fn queue_new_constructs_without_panic() {
        let (_queue, _audio_thread) = make_queue();
    }

    #[kithara::test]
    fn a_control_reads_closed_once_its_queue_is_dropped() {
        let (queue, _audio_thread) = make_queue();
        let control = queue.control();
        assert!(!control.is_closed(), "the queue still owns its mailbox");

        drop(queue);

        assert!(control.is_closed());
    }

    #[kithara::test]
    fn queue_registers_its_resident_players_deck_with_the_session() {
        let mut host = kithara_host::Host::new(kithara_host::HostConfig::offline(pools()).build())
            .expect("offline host");
        let deck = host
            .insert(Queue::new(queue_config()))
            .expect("host registers the queue");
        let deck_id = deck.id();
        assert!(!deck.is_closed());
        host.remove(&deck)
            .expect("the owning host releases the queue");
        assert!(deck.is_closed());
        assert!(matches!(host.remove(&deck),
            Err(PlayError::Session(kithara_play::SessionError::DeckNotFound(refused)))
                if refused == deck_id));
    }

    /// A holder's waker that reports each wake.
    struct Wakes(mpsc::Sender<()>);

    impl Wake for Wakes {
        fn wake(self: Arc<Self>) {
            let _ = self.0.send(());
        }
    }

    #[kithara::test]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    fn a_control_command_runs_when_the_holder_drains_the_queue() {
        let (mut queue, mut rig) = make_queue();
        let control = queue.control();
        let (woke_tx, woke_rx) = mpsc::channel();
        HostedDeck::hold(&mut queue, Waker::from(Arc::new(Wakes(woke_tx))));

        let (control, appended) = {
            // WHY: The append caller is a real thread that Flash does not count as busy.
            let _real_io = kithara_platform::flash::real_io();
            let append = thread::spawn(move || {
                let appended = control.append("https://example.com/a.mp3");
                (control, appended)
            });
            woke_rx
                .recv_timeout(kithara_platform::time::Instant::now() + __hang_detector.remaining())
                .expect("a post wakes the holder");

            assert!(
                queue.control().is_empty(),
                "the command waits for the holder to drain"
            );

            with_outbox(&mut queue, &mut rig, |queue, pass, out| {
                HostedDeck::drain(queue, pass, out);
            });
            while !append.is_finished() {
                hang_tick!("append caller awaits its mailbox answer".to_owned());
                thread::yield_now();
            }
            let (control, appended) = append.join().expect("append thread must not panic");
            hang_reset!();
            (control, appended)
        };
        hang_reset!();
        let id = appended.expect("an open queue appends");
        assert_eq!(
            queue.control().tracks().first().map(|track| track.id),
            Some(id)
        );

        drop(queue);
        assert!(matches!(
            control.append("https://example.com/b.mp3"),
            Err(QueueError::Play(PlayError::Closed))
        ));
    }

    /// A holder's waker that reports each wake to an async waiter.
    struct WakesTask(UnboundedSender<()>);

    impl Wake for WakesTask {
        fn wake(self: Arc<Self>) {
            let _ = self.0.send(());
        }
    }

    /// A load's transitions reach its track on the queue's owner: whatever the
    /// load did meanwhile, the track keeps the status the owner left it with
    /// until the executor holding the queue drains it.
    #[kithara::test(tokio)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
    async fn a_load_reaches_its_track_only_when_the_holder_drains_the_queue() {
        let (mut queue, mut rig) = make_queue();
        let (woke_tx, mut woke_rx) = unbounded_channel();
        rig.dispatcher
            .hold(Waker::from(Arc::new(WakesTask(woke_tx))));
        let id = TrackId::allocate();
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Append {
                position: Position::ZERO,
                id,
                source: concat!(env!("CARGO_MANIFEST_DIR"), "/missing-track.wav").into(),
            },
        )
        .expect("an open queue appends");
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Select {
                id,
                transition: super::super::Transition::None,
            },
        )
        .expect("select queues the load");
        let status = |queue: &Queue<TestPools>| {
            queue
                .control()
                .track(id)
                .expect("the track stays queued")
                .status
        };

        loop {
            hang_tick!("dispatcher load awaits a queue owner wake".to_owned());
            let left = status(&queue);
            let receipt = rig
                .open(Err(kithara_play::LoadRefusal::Source(
                    kithara_audio::TrackFailureKind::Decode {
                        kind: kithara_audio::DecodeErrorKind::Io,
                    },
                )))
                .expect("the dispatcher answers the load");
            timeout(__hang_detector.remaining(), woke_rx.recv())
                .await
                .expect("the load reports to the holder");
            hang_reset!();
            assert_eq!(
                status(&queue),
                left,
                "a load's report waits for the holder to drain the queue"
            );
            with_outbox(&mut queue, &mut rig, |queue, pass, out| {
                HostedDeck::settle(
                    queue,
                    kithara_play::TrackReceipt::Loaded(receipt),
                    pass,
                    out,
                );
            });
            if matches!(status(&queue), TrackStatus::Failed(_)) {
                break;
            }
        }
    }

    #[kithara::test]
    fn a_closed_queue_rejects_mutation() {
        let (mut queue, mut rig) = make_queue();

        with_outbox(&mut queue, &mut rig, |queue, _, out| {
            HostedDeck::close(queue, out)
        })
        .expect("unstarted fixture must close");

        assert!(queue.shutdown.is_cancelled());
        assert!(matches!(
            apply(
                &mut queue,
                &mut rig,
                QueueCommand::Append {
                    position: Position::ZERO,
                    id: TrackId::allocate(),
                    source: "https://example.com/a.mp3".into(),
                }
            ),
            Err(PlayError::Closed)
        ));
        assert!(queue.control().is_empty());
    }

    #[kithara::test]
    fn retained_config_follows_live_queue_controls() {
        let (mut queue, mut rig) = make_queue();
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::SetActionAtItemEnd(ActionAtItemEnd::Pause),
        )
        .expect("action");
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::SetPlaybackOrder(PlaybackOrder::Shuffle),
        )
        .expect("order");
        let mut crossfade = queue.control().crossfade_settings();
        crossfade.duration = 2.0;
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::ConfigureQueue(QueueSettingsChange::Crossfade(crossfade), When::Next),
        )
        .expect("valid crossfade settings");

        let values = queue.config.values();
        assert_eq!(values.action_at_item_end, ActionAtItemEnd::Pause);
        assert_eq!(values.playback_order, PlaybackOrder::Shuffle);
        assert_eq!(values.settings.crossfade(), crossfade);
        assert_eq!(
            queue.control().crossfade_settings().duration,
            crossfade.duration
        );
    }

    /// A queue event is heard only with the view that shows it, in the order
    /// the queue made its changes: a handle that hears a track change reads
    /// that change.
    #[kithara::test]
    fn a_current_item_event_is_published_with_its_committed_queue_view() {
        let (mut queue, _) = make_queue();
        let control = queue.control();
        let id = TrackId::allocate();
        queue.insert_entry(
            id,
            "https://example.com/a.mp3".into(),
            Position::ZERO,
            Placement::Append,
        );
        let mut events = control.subscribe::<PlayerEvent>();
        queue.current = Some(id);
        queue.announce(QueueEvent::CurrentTrackChanged { id: Some(id) });
        assert!(events.try_recv().is_err());
        assert!(control.current().is_none());

        queue.publish();
        assert!(matches!(
            events.try_recv().expect("committed current item").event,
            PlayerEvent::CurrentItemChanged { item: Some(actual) } if actual == id
        ));
        assert_eq!(control.current().expect("published current").id, id);
        queue.publish();
        assert!(
            events.try_recv().is_err(),
            "unchanged current is not re-announced"
        );

        queue.current = None;
        queue.announce(QueueEvent::CurrentTrackChanged { id: None });
        queue.publish();
        assert!(matches!(
            events.try_recv().expect("committed inactive queue").event,
            PlayerEvent::CurrentItemChanged { item: None }
        ));
        assert!(control.current().is_none());
    }

    #[kithara::test]
    fn a_queue_event_is_heard_with_the_view_that_shows_it() {
        let (mut queue, mut rig) = make_queue();
        let control = queue.control();
        let id = TrackId::allocate();
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Append {
                position: Position::ZERO,
                id,
                source: "https://example.com/a.mp3".into(),
            },
        )
        .expect("an open queue appends");
        let mut events = queue.bus.subscribe::<QueueEvent>();

        queue.tracks.set_status(id, TrackStatus::Consumed);
        assert!(
            events.try_recv().is_err(),
            "a change is not heard before the queue publishes it"
        );
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::SetRepeat(RepeatMode::All),
        )
        .expect("repeat");

        let heard: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|envelope| envelope.event)
            .collect();
        assert!(
            matches!(
                heard.as_slice(),
                [
                    QueueEvent::TrackStatusChanged {
                        id: changed,
                        status: TrackStatus::Consumed,
                    },
                    QueueEvent::RepeatModeChanged {
                        mode: QueueRepeatMode::All,
                    },
                ] if *changed == id
            ),
            "the publish announces both changes in order: {heard:?}"
        );
        assert_eq!(
            control.track(id).map(|track| track.status),
            Some(TrackStatus::Consumed)
        );
        assert_eq!(control.repeat_mode(), RepeatMode::All);
    }

    #[kithara::test]
    fn cached_position_unknown_after_construction() {
        let (queue, _audio_thread) = make_queue();
        assert_eq!(
            queue.queue_snapshot().track.map(|track| track.position),
            None
        );
        assert_eq!(queue.control().position_seconds(), None);
    }

    fn armed_queue(id: TrackId) -> (Queue<TestPools>, DeckRig<TestPools>, Seq, Seq) {
        let (mut queue, mut rig) = make_queue();
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Append {
                position: Position::ZERO,
                id,
                source: concat!(env!("CARGO_MANIFEST_DIR"), "/missing-track.wav").into(),
            },
        )
        .expect("append transition fixture");
        let seq = apply(
            &mut queue,
            &mut rig,
            QueueCommand::Select {
                id,
                transition: super::super::Transition::None,
            },
        )
        .expect("select queues its load")
        .expect("load sequence");
        let index = queue.incoming_index(id).expect("incoming track");
        queue.active.get_mut(index).expect("active incoming").role = Role::Incoming;
        queue.pending_handover = Some(PendingHandover {
            seq,
            item: id,
            slot: queue.active.get(index).expect("incoming slot").slot,
            replacement: queue.active.is_replacement(index),
            reason: crate::AdvanceReason::UserSelect,
        });
        let other = TrackId(id.0 + 1);
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Append {
                position: Position::ZERO,
                id: other,
                source: concat!(env!("CARGO_MANIFEST_DIR"), "/other-missing-track.wav").into(),
            },
        )
        .expect("append unrelated track");
        let unrelated = with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            queue.load_track(other, Role::Preloaded, Some(pass.output), out)
        })
        .expect("preload unrelated track")
        .expect("unrelated load sequence");
        assert_ne!(seq, unrelated);
        (queue, rig, seq, unrelated)
    }

    #[kithara::test]
    fn atomic_cached_position_unknown_loads_none() {
        let (queue, _rig) = make_queue();
        assert_eq!(queue.control().position_seconds(), None);
    }

    #[kithara::test]
    fn atomic_cached_position_round_trip_zero() {
        let (mut queue, mut rig) = make_queue();
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Seek { to: Position::ZERO },
        )
        .expect("seek to zero");
        queue.publish();
        assert_eq!(queue.control().position_seconds(), Some(0.0));
    }

    #[kithara::test(tokio)]
    async fn crossfade_arm_is_armed_for_matches_track() {
        let (queue, _rig, _, _) = armed_queue(TrackId(2));
        assert!(queue.target.is_some_and(|target| target.to == TrackId(2)));
        assert!(!queue.target.is_some_and(|target| target.to == TrackId(3)));
    }

    #[kithara::test]
    fn cached_position_round_trips_through_queue() {
        let (mut queue, mut rig) = make_queue();
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Seek {
                to: Position::try_from_secs_f64(12.5).expect("finite position"),
            },
        )
        .expect("seek to held position");
        queue.publish();
        assert_eq!(queue.control().position_seconds(), Some(12.5));
    }

    #[kithara::test]
    fn crossfade_arm_disarmed_after_construction() {
        let (queue, _rig) = make_queue();
        assert_eq!(queue.target.map(|target| target.to), None);
    }

    #[kithara::test(tokio)]
    async fn crossfade_arm_take_only_disarms_matching_track() {
        let (mut queue, _rig, seq, unrelated) = armed_queue(TrackId(9));
        assert_eq!(queue.target.map(|target| target.to), Some(TrackId(9)));
        queue
            .transition_applied(
                unrelated,
                queue.incoming_index(TrackId(9)).expect("incoming"),
                SessionFrame::new(128),
            )
            .expect("unrelated receipt");
        assert_eq!(queue.target.map(|target| target.to), Some(TrackId(9)));
        assert_eq!(queue.current, None);
        queue
            .transition_applied(
                seq,
                queue.incoming_index(TrackId(9)).expect("incoming"),
                SessionFrame::new(128),
            )
            .expect("matching receipt");
        assert_eq!(queue.current, Some(TrackId(9)));
        assert!(queue.target.is_none());
    }

    #[kithara::test]
    fn the_configured_prefetch_lead_reaches_the_player() {
        let mut config = queue_config();
        config.preload_lead = Duration::from_secs(8);
        let queue = Queue::new(config);
        assert!((queue.config.preload_lead.as_secs_f32() - 8.0).abs() < f32::EPSILON);
    }

    #[kithara::test(native)]
    fn queue_hands_its_session_the_resident_players_sync_attachment() {
        use kithara_host::{DeckId, Host, HostConfig, HostOwner};

        let expected = DeckId::allocate().expect("fixture deck id");
        let queue = Queue::new(queue_config());
        let control = queue.control();
        let append = control
            .postbox
            .post(QueueCommand::Append {
                position: Position::ZERO,
                id: TrackId(42),
                source: "https://example.com/attachment.mp3".into(),
            })
            .expect("post to the queue being registered");
        let _host = Host::layered(HostConfig::offline(pools()).build(), move |mut core| {
            core.register(expected, Box::new(queue))
                .expect("register the queue");
            answered(append).expect("registration drains the same queue's control");
            let mut attachments = Vec::new();
            core.each_deck(&mut |id, deck, _, _| {
                attachments.push(id);
                assert_eq!(deck.tracks.records().len(), 1);
                assert_eq!(deck.tracks.records()[0].id, TrackId(42));
            });
            assert_eq!(attachments, [expected]);
            core.with_deck(expected, &mut |deck, _, _| {
                assert_eq!(deck.tracks.records()[0].id, TrackId(42));
                assert_eq!(control.tracks().len(), 1);
            })
            .expect("the exact registration identity routes back to the same queue");
            core
        })
        .expect("offline session attachment");
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
    fn answered(
        mut ticket: kithara_command::Ticket<QueueError>,
    ) -> Result<(), kithara_command::Refused<QueueError>> {
        let _real_io = kithara_platform::flash::real_io();
        loop {
            let mut answer = None;
            hang_park!(
                |remaining: Duration| {
                    let now = kithara_platform::time::Instant::now();
                    answer = Some(ticket.wait_until(now + remaining));
                },
                "queue fixture owner has not answered its ticket".to_owned()
            );
            match answer {
                Some(Ok(answer)) => {
                    hang_reset!();
                    return answer;
                }
                Some(Err(pending)) => ticket = pending,
                None => unreachable!("the watchdog executes its wait closure"),
            }
        }
    }

    #[kithara::test]
    fn queue_control_post_preserves_order_and_reports_the_owner_outcome() {
        let (mut queue, mut rig) = make_queue();
        let control = queue.control();
        let first = TrackId(41);
        let second = TrackId(42);
        let append = control
            .post(QueueCommand::Append {
                position: Position::ZERO,
                id: first,
                source: "https://example.com/first.mp3".into(),
            })
            .expect("post before the owner runs");
        let insert = control
            .post(QueueCommand::Insert {
                id: second,
                source: "https://example.com/second.mp3".into(),
                after: Some(first),
            })
            .expect("the second post shares the first mailbox");
        let unknown = TrackId(99);
        let refused = control
            .post(QueueCommand::Remove(unknown))
            .expect("a post does not claim owner acceptance");
        assert!(
            control.is_empty(),
            "posting must not mutate the published view"
        );
        let Err(append) = append.wait_until(kithara_platform::time::Instant::now()) else {
            panic!("the owner has not run yet");
        };
        with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            HostedDeck::drain(queue, pass, out);
        });
        answered(append).expect("first command applied");
        answered(insert).expect("insertion observes the earlier append");
        assert!(matches!(
            answered(refused),
            Err(kithara_command::Refused::Owner(QueueError::UnknownTrackId(id))) if id == unknown
        ));
        assert_eq!(
            control
                .tracks()
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [first, second]
        );
        let unanswered = control.post(QueueCommand::Tick).expect("live mailbox");
        drop(queue);
        assert!(matches!(
            answered(unanswered),
            Err(kithara_command::Refused::Unanswered)
        ));
        assert!(matches!(
            control.post(QueueCommand::Tick),
            Err(QueueError::Play(PlayError::Closed))
        ));
    }

    #[kithara::test]
    fn queue_control_rejects_mutation_after_close() {
        let (mut queue, mut rig) = make_queue();
        let control = queue.control();
        let close = control
            .postbox
            .post(QueueCommand::Close)
            .expect("post close");
        with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            HostedDeck::drain(queue, pass, out);
        });
        answered(close).expect("unstarted fixture must close");
        assert!(queue.shutdown.is_cancelled());
        let append = control
            .postbox
            .post(QueueCommand::Append {
                position: Position::ZERO,
                id: TrackId(1),
                source: "https://example.com/a.mp3".into(),
            })
            .expect("owner still holds the closed queue's mailbox");
        with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            HostedDeck::drain(queue, pass, out);
        });
        assert!(matches!(
            answered(append),
            Err(kithara_command::Refused::Owner(QueueError::Play(
                PlayError::Closed
            )))
        ));
        assert!(control.is_empty());
        assert!(queue.tracks.records().is_empty());
    }

    #[kithara::test]
    fn close_waits_for_an_admitted_queue_mutation() {
        let (mut queue, mut rig) = make_queue();
        let control = queue.control();
        let mutation = control
            .postbox
            .post(QueueCommand::Append {
                position: Position::ZERO,
                id: TrackId(1),
                source: "https://example.com/a.mp3".into(),
            })
            .expect("admit mutation");
        let mut admitted = queue.mailbox.drain();
        let post = admitted.next().expect("one admitted mutation");
        assert!(admitted.next().is_none());
        let close = control
            .postbox
            .post(QueueCommand::Close)
            .expect("post close after admission");
        assert!(mutation.seq() < close.seq());
        assert!(
            !queue.shutdown.is_cancelled(),
            "close cannot overtake the admitted mutation"
        );
        assert!(queue.tracks.records().is_empty());

        let result = with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            queue.apply_command(post.command, Some(pass.output), out)
        });
        assert!(result.is_ok());
        post.answer.answer(result.map(|_| ()));
        answered(mutation).expect("admitted mutation remains open");
        assert_eq!(queue.tracks.records().len(), 1);
        assert_eq!(queue.tracks.records()[0].id, TrackId(1));
        assert!(
            !queue.shutdown.is_cancelled(),
            "mutation completes before close"
        );

        with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            HostedDeck::drain(queue, pass, out);
        });
        answered(close).expect("unstarted fixture must close");
        assert!(queue.shutdown.is_cancelled());
        assert!(matches!(queue.ensure_open(), Err(PlayError::Closed)));
    }
}
