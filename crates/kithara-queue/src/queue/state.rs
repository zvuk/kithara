use kithara_assets::{AssetStore, StorageBackend};
use kithara_bufpool::HasPool;
use kithara_command::mailbox;
use kithara_events::{EventBus, TrackId};
use kithara_platform::{CancelScope, CancelToken, tokio::runtime::Handle as RuntimeHandle};
use kithara_play::{PlayError, PlayerEvent, PlayerFactory, Position, TrackFactory};
use kithara_signal::{FrameCount, SessionFrame};

use super::{
    command::{QueueMailbox, QueuePostbox},
    slots::{Role, Slots},
    types::Target,
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
    pub(super) held_position: Option<Position>,
    pub(super) target: Option<Target>,
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
                config
                    .runtime
                    .clone()
                    .or_else(|| RuntimeHandle::try_current().ok()),
                postbox.clone(),
            )
        });
        let mut navigation = NavigationState::new(config.max_history_size);
        navigation.set_playback_order(config.playback_order, &[]);
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
            .find(|active| active.role == Role::Current)
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
        self.events.extend(self.tracks.drain_events());
        let snapshot = self.queue_snapshot();
        let previous = self.view.read().deck.mix;
        let mix = snapshot.deck.mix;
        self.view.publish(snapshot);
        if previous.volume() != mix.volume() {
            self.bus.publish(PlayerEvent::VolumeChanged {
                volume: f32::from(mix.volume()),
            });
        }
        if previous.muted() != mix.muted() {
            self.bus
                .publish(PlayerEvent::MuteChanged { muted: mix.muted() });
        }
        for event in self.events.drain(..) {
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
        sync::{Arc, mpsc},
        task::{Wake, Waker},
    };

    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_command::{Seq, When};
    use kithara_config::Config;
    use kithara_events::{Envelope, EventReceiver, TrackId};
    use kithara_platform::{
        thread,
        time::{Duration, WallInstant, timeout},
        tokio::sync::mpsc::{UnboundedSender, unbounded_channel},
    };
    use kithara_play::{
        DeckMixerConfig, DeckPass, HostedDeck, Outbox, PlayError, PlayWorker, PlayWorkerConfig,
        Player, ResourcePrep,
        mock::{self, DeckRig},
    };
    use kithara_signal::{FrameCount, SessionFrame};
    use kithara_test_utils::kithara;

    use super::{super::QueueCommand, *};
    use crate::{
        QueueSettingsChange,
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
            let remaining = deadline.saturating_duration_since(WallInstant::now());
            if remaining.is_zero() {
                return false;
            }
            match timeout(remaining, rx.recv()).await {
                Ok(Ok(Envelope { event: ev, .. })) if matches(&ev) => return true,
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
    fn a_control_command_runs_when_the_holder_drains_the_queue() {
        let (mut queue, mut rig) = make_queue();
        let control = queue.control();
        let (woke_tx, woke_rx) = mpsc::channel();
        HostedDeck::hold(&mut queue, Waker::from(Arc::new(Wakes(woke_tx))));

        let append = thread::spawn(move || {
            let appended = control.append("https://example.com/a.mp3");
            (control, appended)
        });
        woke_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a post wakes the holder");
        assert!(
            queue.control().is_empty(),
            "the command waits for the holder to drain"
        );

        with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            HostedDeck::drain(queue, pass, out);
        });
        let (control, appended) = append.join().expect("append thread must not panic");
        let id = appended.expect("an open queue appends");
        assert_eq!(
            queue.control().tracks().first().map(|track| track.id),
            Some(id)
        );

        drop(queue);
        assert!(matches!(
            control.append("https://example.com/b.mp3"),
            Err(crate::QueueError::Play(PlayError::Closed))
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
                id,
                source: "/kithara/missing-track.wav".into(),
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
            let left = status(&queue);
            let receipt = rig
                .open(Err(kithara_play::LoadRefusal::Source(
                    kithara_audio::TrackFailureKind::Decode {
                        kind: kithara_audio::DecodeErrorKind::Io,
                    },
                )))
                .expect("the dispatcher answers the load");
            timeout(Duration::from_secs(5), woke_rx.recv())
                .await
                .expect("the load reports to the holder");
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
    fn a_queue_event_is_heard_with_the_view_that_shows_it() {
        let (mut queue, mut rig) = make_queue();
        let control = queue.control();
        let id = TrackId::allocate();
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Append {
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

    #[kithara::test]
    fn select_phase_idle_after_construction() {
        let (queue, _audio_thread) = make_queue();
        assert!(queue.target.is_none());
    }
}
