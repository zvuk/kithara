use std::num::{NonZeroU32, NonZeroUsize};

#[cfg(not(target_arch = "wasm32"))]
use kithara::audio::AudioReader;
use kithara::{
    decode::GaplessMode,
    effects::eq::EqBandConfig,
    events::{EventReceiver, TrackId},
    host::{DeckControl, HostConfig, HostOwned, HostSettings},
    platform::{
        maybe_send::MaybeSend,
        sync::{Arc, Mutex},
        time::WallInstant,
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{
        CrossfadeSettings, HostedDeck, PlayWorker, PlayWorkerConfig, PlayerEvent, PlayerFactory,
        Resource, ResourcePrep, TrackFactory, TrackSettings,
    },
    queue::{Queue, QueueConfig, QueueControl, QueueError, QueueSettings, TrackSource, Transition},
    warp::WarpConfig,
};
use kithara_render::bridge::{DeckSnapshot, RtMetricsSnapshot};

use super::{
    OfflineHostHarness,
    host::{ObservedDeck, offline_pools},
};
use crate::{
    assets_ext::memory_asset_store,
    bufpool_ext::{TestPools, pools},
    event::TestEvent,
};

/// Product queue on an offline Host, rendered deterministically by the test.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct OfflinePlayer<F: TrackFactory<TestPools> = PlayerFactory> {
    events: Mutex<EventReceiver<TestEvent>>,
    host: OfflineHostHarness<TestPools>,
    queue: HostOwned<ObservedDeck<Queue<TestPools, F>>>,
    snapshot: Arc<Mutex<DeckSnapshot>>,
    worker: PlayWorker<TestPools>,
    #[field(get = resource_prep)]
    prep: ResourcePrep<TestPools>,
    #[cfg(not(target_arch = "wasm32"))]
    pcm_decks: Mutex<Vec<crate::mock::PcmDeck>>,
}

/// Product player settings a test varies.
#[derive(Clone, bon::Builder)]
pub struct OfflinePlayerOptions {
    #[builder(default = CrossfadeSettings::default().duration)]
    crossfade_duration: f32,
    #[builder(default)]
    gapless: bool,
    eq_layout: Option<Vec<EqBandConfig>>,
    #[builder(default)]
    gapless_mode: GaplessMode,
    /// Make audio-thread reads block on a producer-ring underrun instead of
    /// zero-filling. Suites that read the rendered PCM itself opt in, because
    /// a zero-filled range is indistinguishable from rendered silence:
    /// blocking trades an underrun for waiting on decode, so under a tight
    /// `hang_timeout_secs` a slow decode becomes a hang panic instead of
    /// inserted silence.
    #[builder(default)]
    block_on_underrun: bool,
    warp: Option<WarpConfig>,
    output_block_frames: Option<NonZeroU32>,
    response_budget_frames: Option<NonZeroUsize>,
}

/// Build a paused queue with crossfade disabled for deterministic offline tests.
pub async fn offline_queue_fixture(sample_rate: u32) -> (OfflinePlayer, QueueControl<TestPools>) {
    offline_queue_fixture_with_options(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        sample_rate,
    )
    .await
}

/// Build a paused queue from explicit product player options.
pub async fn offline_queue_fixture_with_options(
    options: OfflinePlayerOptions,
    sample_rate: u32,
) -> (OfflinePlayer, QueueControl<TestPools>) {
    let harness = OfflinePlayer::with_sample_rate(options, sample_rate).await;
    let queue = harness.queue.control().clone();
    (harness, queue)
}

impl OfflinePlayer {
    /// Product defaults on the Host `session` describes.
    pub async fn new(session: HostConfig<TestPools>) -> Self {
        Self::with_options(OfflinePlayerOptions::builder().build(), session).await
    }

    /// `options` on a default offline Host at `sample_rate`.
    pub async fn with_sample_rate(options: OfflinePlayerOptions, sample_rate: u32) -> Self {
        let sample_rate =
            NonZeroU32::new(sample_rate).expect("offline player sample rate must be non-zero");
        let session = HostConfig::offline(pools())
            .settings(HostSettings::builder().sample_rate(sample_rate).build())
            .maybe_max_block_frames(options.output_block_frames)
            .build();
        Self::with_options(options, session).await
    }

    /// `options` on the Host `session` describes.
    ///
    /// # Panics
    ///
    /// Panics if the product offline Host cannot be created.
    pub async fn with_options(
        options: OfflinePlayerOptions,
        session: HostConfig<TestPools>,
    ) -> Self {
        Self::with_factory(options, session, PlayerFactory).await
    }
}

impl<F> OfflinePlayer<F>
where
    F: TrackFactory<TestPools> + MaybeSend + 'static,
    F::Track: MaybeSend,
{
    pub async fn with_factory(
        options: OfflinePlayerOptions,
        session: HostConfig<TestPools>,
        factory: F,
    ) -> Self {
        let pools = offline_pools(&session).clone();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
        let track = options
            .warp
            .as_ref()
            .map_or_else(TrackSettings::default, |warp| {
                TrackSettings::builder()
                    .speed(warp.speed())
                    .keylock(warp.keylock())
                    .backend(warp.backend())
                    .build()
            });
        let prep = ResourcePrep::builder()
            .worker(worker.clone())
            .gapless_mode(options.gapless_mode)
            .block_on_underrun(options.block_on_underrun)
            .maybe_warp(options.warp)
            .maybe_response_budget_frames(options.response_budget_frames)
            .build();
        let settings = QueueSettings::builder()
            .gapless(options.gapless)
            .crossfade(CrossfadeSettings {
                duration: options.crossfade_duration,
                ..CrossfadeSettings::default()
            })
            .build();
        let queue = Queue::new(
            QueueConfig::with_factory(factory)
                .prep(prep.clone())
                .track(track)
                .settings(settings)
                .store(memory_asset_store())
                .build(),
        );
        let events = queue.control().subscribe();
        let host = OfflineHostHarness::new(session)
            .await
            .unwrap_or_else(|error| panic!("create product offline Host: {error}"));

        let snapshot = Arc::new(Mutex::new(DeckSnapshot::default()));
        let queue = host
            .insert(ObservedDeck {
                inner: queue,
                snapshot: snapshot.clone(),
            })
            .await
            .unwrap_or_else(|error| panic!("insert product offline queue: {error}"));
        if let Some(layout) = options.eq_layout {
            queue
                .set_eq_layout(layout)
                .unwrap_or_else(|error| panic!("configure product offline queue EQ: {error}"));
        }
        Self {
            snapshot,
            events: Mutex::new(events),
            host,
            queue,
            worker,
            prep,
            #[cfg(not(target_arch = "wasm32"))]
            pcm_decks: Mutex::new(Vec::new()),
        }
    }

    /// The resident queue's published control and observations.
    #[must_use]
    pub fn player(&self) -> &QueueControl<TestPools> {
        self.queue.control()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn pcm_deck(&self, reader: Box<dyn AudioReader>) -> TrackSource<TestPools> {
        let deck = crate::mock::PcmDeck::new(reader);
        let source = deck.source();
        self.pcm_decks.lock().push(deck);
        source
    }

    delegate::delegate! {
        to self {
            /// Decode worker this player pulls from, for opening resources
            /// beside it.
            #[field(&worker)]
            pub const fn worker(&self) -> &PlayWorker<TestPools>;
            /// Offline Host owning this player, for probes the Host installs.
            #[field(&host)]
            pub const fn host(&self) -> &OfflineHostHarness<TestPools>;
        }
    }

    /// Set the transition duration used by the next load.
    pub async fn set_fade_duration(&self, seconds: f32) {
        self.with_queue(move |control| {
            control.set_crossfade_settings(CrossfadeSettings {
                duration: seconds,
                ..control.crossfade_settings()
            })
        })
        .await
        .unwrap_or_else(|error| panic!("configure offline crossfade: {error}"));
    }

    /// Set the player volume used by subsequent offline renders.
    ///
    /// # Errors
    /// Returns the player's refusal of the change.
    pub async fn set_volume(&self, volume: f32) -> Result<(), QueueError> {
        self.with_queue(move |control| control.set_volume(volume))
            .await
    }

    /// Current playback position in seconds.
    #[must_use]
    pub fn position(&self) -> f64 {
        self.queue.position_seconds().unwrap_or_default()
    }

    pub fn metrics(&self) -> RtMetricsSnapshot {
        self.snapshot.lock().metrics
    }

    pub fn deck_snapshot(&self) -> DeckSnapshot {
        self.snapshot.lock().clone()
    }

    /// Issues queue control calls from the host owner thread.
    pub async fn with_queue<R>(
        &self,
        use_queue: impl FnOnce(&QueueControl<TestPools>) -> R + MaybeSend + 'static,
    ) -> R
    where
        R: MaybeSend + 'static,
    {
        assert!(
            !self.queue.is_closed(),
            "the offline harness queue is closed; command a resident queue"
        );
        self.run(self.queue.control(), use_queue).await
    }

    /// Issues a control call on `control` from the host owner thread, as the
    /// app would.
    pub async fn run<C, R>(&self, control: &C, f: impl FnOnce(&C) -> R + MaybeSend + 'static) -> R
    where
        C: Clone + MaybeSend + 'static,
        R: MaybeSend + 'static,
    {
        let control = control.clone();
        self.host.run(move || f(&control)).await
    }

    /// # Panics
    ///
    /// Panics if the Host rejects the facade.
    pub async fn insert<P>(&self, player: P) -> HostOwned<P>
    where
        P: DeckControl + HostedDeck<TestPools> + MaybeSend + 'static,
        P::Control: MaybeSend,
    {
        self.host
            .insert(player)
            .await
            .unwrap_or_else(|error| panic!("insert player facade into offline Host: {error}"))
    }

    pub async fn insert_control<P>(&self, player: P) -> P::Control
    where
        P: DeckControl + HostedDeck<TestPools> + MaybeSend + 'static,
        P::Control: Clone + MaybeSend,
    {
        self.insert(player).await.control().clone()
    }

    /// Load one resource and start playback.
    ///
    /// # Panics
    ///
    /// Panics if the product player rejects the resource.
    pub async fn load_and_fadein(&self, source: impl Into<TrackSource<TestPools>>) {
        let source = source.into();
        self.with_queue(move |control| {
            let id = control
                .append(source)
                .unwrap_or_else(|error| panic!("append offline queue item: {error}"));
            control
                .select(id, Transition::Crossfade)
                .unwrap_or_else(|error| panic!("select offline queue item: {error}"));
            control.play();
        })
        .await;
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn load_config(&self, config: kithara::play::ResourceConfig<TestPools>) -> TrackId {
        let mut events = self.player().subscribe();
        let id = self
            .with_queue(move |control| {
                let id = control
                    .append(TrackSource::Config(Box::new(config)))
                    .expect("append configured fixture source");
                control
                    .select(id, Transition::Crossfade)
                    .expect("select configured fixture source");
                control.play();
                id
            })
            .await;
        crate::waits::wait_for_loader_done_event(
            &mut events,
            self.player(),
            id,
            super::loader::LOCAL_LOAD_DEADLINE,
        )
        .await
        .expect("configured fixture source is ready");
        id
    }

    /// Render until selection is committed by the queue, not merely prepared by its loader.
    pub async fn render_until_current(
        &self,
        id: TrackId,
        block_frames: usize,
        deadline: WallInstant,
    ) {
        while self.player().current().map(|entry| entry.id) != Some(id) {
            assert!(
                WallInstant::now() < deadline,
                "queue did not commit current track {id:?} before the render deadline"
            );
            self.render(block_frames).await;
            let _ = self.tick_and_drain().await;
        }
    }

    /// Seek through the product player. The product runtime owns segments.
    ///
    /// # Panics
    ///
    /// Panics if the product player rejects the seek.
    pub async fn seek(&self, seconds: f64) {
        self.with_queue(move |control| control.seek(seconds))
            .await
            .unwrap_or_else(|error| panic!("seek offline player: {error}"));
    }

    /// Render `frames` of interleaved stereo audio through the product Host.
    /// A resident player then publishes what the block produced, as the app
    /// update loop would.
    pub async fn render(&self, frames: usize) -> Vec<f32> {
        let output = self.host.render(frames).await;
        self.tick_player().await;
        output
    }

    /// Pump the player's notification ringbuf and drain `PlayerEvent`s
    /// from the bus subscriber.
    pub async fn tick_and_drain(&self) -> Vec<PlayerEvent> {
        self.tick_player().await;
        self.drain_events()
            .into_iter()
            .filter_map(|event| match event {
                TestEvent::Player(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    /// Drain the product event stream into the scenario observation tags.
    pub async fn take_notification_kinds(&self) -> Vec<NotificationKind> {
        self.tick_player().await;
        self.drain_events()
            .into_iter()
            .filter_map(|event| match event {
                TestEvent::Player(PlayerEvent::PlaybackStarted { .. }) => {
                    Some(NotificationKind::PlaybackStarted)
                }
                TestEvent::Player(
                    PlayerEvent::ItemDidPlayToEnd { .. } | PlayerEvent::ItemDidFail { .. },
                ) => Some(NotificationKind::PlaybackStopped),
                _ => None,
            })
            .collect()
    }

    pub async fn close(self) {
        let Self {
            events,
            host,
            queue,
            worker,
            prep,
            #[cfg(not(target_arch = "wasm32"))]
            pcm_decks,
            snapshot,
        } = self;
        drop(events);
        drop(snapshot);
        drop(queue);
        drop(worker);
        host.close().await;
        drop(prep);
        #[cfg(not(target_arch = "wasm32"))]
        drop(pcm_decks);
    }

    fn drain_events(&self) -> Vec<TestEvent> {
        let mut events = Vec::new();
        let mut rx = self.events.lock();
        loop {
            match rx.try_recv() {
                Ok(envelope) => events.push(envelope.event),
                Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        events
    }

    /// Settles receipts and publishes observations through the queue owner.
    async fn tick_player(&self) {
        self.with_queue(QueueControl::tick)
            .await
            .unwrap_or_else(|error| panic!("tick offline queue: {error}"));
    }
}

/// Test observation tags retained for scenario assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    PlaybackStarted,
    PlaybackStopped,
}

/// Thin wrapper around [`Resource::from_reader`] for tests.
pub fn resource_from_reader<R>(reader: R) -> Resource
where
    R: AudioReader + 'static,
{
    Resource::from_reader(reader, None)
}

/// Thin wrapper around [`Resource::from_reader`] with an explicit source tag.
pub fn resource_from_reader_with_src<R, S>(reader: R, src: S) -> Resource
where
    R: AudioReader + 'static,
    S: Into<Arc<str>>,
{
    Resource::from_reader(reader, Some(src.into()))
}
