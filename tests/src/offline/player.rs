use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    audio::AudioReader,
    decode::GaplessMode,
    effects::eq::EqBandConfig,
    events::{EventReceiver, TrackId},
    host::{HostConfig, HostOwned},
    platform::{
        maybe_send::MaybeSend,
        sync::{Arc, Mutex},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{
        CrossfadeSettings, DEFAULT_CROSSFADE_DURATION, PlayWorker, PlayWorkerConfig, PlayerConfig,
        PlayerEvent, PlayerImpl, Resource,
        bridge::RtMetricsSnapshot,
        player::{Player, PlayerControl, PlayerControlSource},
    },
    queue::{Queue, QueueConfig, QueueControl},
    warp::WarpConfig,
};

use super::{OfflineHostHarness, host::offline_pools};
use crate::{
    bufpool_ext::{TestPools, pools},
    event::TestEvent,
};

/// Product Player on an offline Host, rendered deterministically by the test.
///
/// The player enters the Host on the first render or control call. Until
/// then a test may [`take_player`](Self::take_player) it into another facade,
/// such as a queue, and insert that facade instead.
pub struct OfflinePlayer {
    events: Mutex<EventReceiver<TestEvent>>,
    host: OfflineHostHarness<TestPools>,
    slot: Mutex<PlayerSlot>,
    player_control: PlayerControl<TestPools>,
    worker: PlayWorker<TestPools>,
}

/// Where the harness player lives.
enum PlayerSlot {
    /// Built, not yet in the Host.
    Pending(Box<PlayerImpl<TestPools>>),
    /// In the Host, owned by this harness.
    Resident,
    /// Handed to another facade through [`OfflinePlayer::take_player`].
    Transferred,
}

/// Product player settings a test varies.
#[derive(Clone, bon::Builder)]
pub struct OfflinePlayerOptions {
    #[builder(default = DEFAULT_CROSSFADE_DURATION)]
    crossfade_duration: f32,
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
    let crossfade_duration = options.crossfade_duration;
    let harness = OfflinePlayer::with_sample_rate(options, sample_rate).await;
    let config = QueueConfig::builder()
        .player(harness.take_player())
        .crossfade_settings(CrossfadeSettings {
            duration: crossfade_duration,
            ..CrossfadeSettings::default()
        })
        .build();
    let queue = harness.insert_control(Queue::new(config)).await;
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
            .sample_rate(sample_rate)
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
        let sample_rate = session.sample_rate();
        let pools = offline_pools(&session).clone();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .crossfade_duration(options.crossfade_duration)
                .gapless_mode(options.gapless_mode)
                .block_on_underrun(options.block_on_underrun)
                .sample_rate(sample_rate)
                .worker(worker.clone())
                .maybe_eq_layout(options.eq_layout)
                .maybe_warp(options.warp)
                .maybe_response_budget_frames(options.response_budget_frames)
                .build(),
        );
        let player_control = player.control();
        let events = player.subscribe();
        let host = OfflineHostHarness::new(session)
            .await
            .unwrap_or_else(|error| panic!("create product offline Host: {error}"));

        Self {
            events: Mutex::new(events),
            host,
            slot: Mutex::new(PlayerSlot::Pending(Box::new(player))),
            player_control,
            worker,
        }
    }

    delegate::delegate! {
        to self {
            #[field(&player_control)]
            pub const fn player(&self) -> &PlayerControl<TestPools>;
            /// Decode worker this player pulls from, for opening resources
            /// beside it.
            #[field(&worker)]
            pub const fn worker(&self) -> &PlayWorker<TestPools>;
            /// Offline Host owning this player, for probes the Host installs.
            #[field(&host)]
            pub const fn host(&self) -> &OfflineHostHarness<TestPools>;
        }
        to self.player_control {
            /// Set the transition duration used by the next load.
            #[call(set_crossfade_duration)]
            pub fn set_fade_duration(&self, seconds: f32);
            /// Set the player volume used by subsequent offline renders.
            pub fn set_volume(&self, volume: f32);
        }
    }

    /// Snapshot the real-time counters owned by this player slot.
    #[must_use]
    pub fn metrics(&self) -> RtMetricsSnapshot {
        self.player_control.rt_metrics().unwrap_or_default()
    }

    /// Current playback position in seconds.
    #[must_use]
    pub fn position(&self) -> f64 {
        self.player_control.position_seconds().unwrap_or_default()
    }

    /// Hands the not yet inserted player to another facade.
    ///
    /// # Panics
    ///
    /// Panics if the player already entered the Host or was taken.
    pub fn take_player(&self) -> PlayerImpl<TestPools> {
        match std::mem::replace(&mut *self.slot.lock(), PlayerSlot::Transferred) {
            PlayerSlot::Pending(player) => *player,
            PlayerSlot::Resident | PlayerSlot::Transferred => {
                panic!("offline harness player was already inserted or transferred")
            }
        }
    }

    /// Issues player control calls from the host owner thread, as the app
    /// would.
    pub async fn with_player<R>(
        &self,
        use_player: impl FnOnce(&PlayerControl<TestPools>) -> R + MaybeSend + 'static,
    ) -> R
    where
        R: MaybeSend + 'static,
    {
        self.ensure_player_inserted().await;
        self.run(&self.player_control, use_player).await
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
    /// Panics if the player was transferred to another facade.
    pub fn set_host_level(&self, level: f32) {
        match &*self.slot.lock() {
            PlayerSlot::Pending(player) => player.set_host_level(level),
            PlayerSlot::Resident | PlayerSlot::Transferred => {
                panic!("offline harness player is no longer pending")
            }
        }
    }

    /// # Panics
    ///
    /// Panics if the Host rejects the facade.
    pub async fn insert<P>(&self, player: P) -> HostOwned<P>
    where
        P: PlayerControlSource<Schema = TestPools> + MaybeSend + 'static,
        P::Control: MaybeSend,
    {
        self.host
            .insert(player)
            .await
            .unwrap_or_else(|error| panic!("insert player facade into offline Host: {error}"))
    }

    pub async fn insert_control<P>(&self, player: P) -> P::Control
    where
        P: PlayerControlSource<Schema = TestPools> + MaybeSend + 'static,
        P::Control: MaybeSend,
    {
        self.insert(player).await.control().clone()
    }

    /// Load one resource and start playback.
    ///
    /// # Panics
    ///
    /// Panics if the product player rejects the resource.
    pub async fn load_and_fadein(&self, resource: Resource) {
        self.with_player(move |control| {
            control.reserve_slots(1);
            control
                .replace_item(0, resource, TrackId::allocate())
                .expect("replace offline player item");
            control.play();
        })
        .await;
    }

    /// Seek through the product player. The product runtime owns seek epochs.
    ///
    /// # Panics
    ///
    /// Panics if the product player rejects the seek.
    pub fn seek(&self, seconds: f64) {
        self.player_control
            .seek_seconds(seconds)
            .unwrap_or_else(|error| panic!("seek offline player: {error}"));
    }

    /// Render `frames` of interleaved stereo audio through the product Host.
    /// A resident player then publishes what the block produced, as the app
    /// update loop would.
    pub async fn render(&self, frames: usize) -> Vec<f32> {
        let resident = self.ensure_player_inserted().await;
        let output = self.host.render(frames).await;
        if resident {
            self.run(&self.player_control, PlayerControl::process_notifications)
                .await;
        }
        output
    }

    /// Pump the player's notification ringbuf and drain `PlayerEvent`s
    /// from the bus subscriber.
    pub async fn tick_and_drain(&self) -> Vec<PlayerEvent> {
        self.run(&self.player_control, PlayerControl::process_notifications)
            .await;
        self.drain_events()
            .into_iter()
            .filter_map(|event| match event {
                TestEvent::Player(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    /// Drain the product event stream into the scenario observation tags.
    pub fn take_notification_kinds(&self) -> Vec<NotificationKind> {
        self.player_control.process_notifications();
        self.drain_events()
            .into_iter()
            .filter_map(|event| match event {
                TestEvent::Player(PlayerEvent::PlaybackStarted { .. }) => {
                    Some(NotificationKind::PlaybackStarted)
                }
                TestEvent::Player(
                    PlayerEvent::ItemDidPlayToEnd { .. } | PlayerEvent::ItemDidFail { .. },
                ) => Some(NotificationKind::PlaybackStopped),
                TestEvent::Player(PlayerEvent::PrefetchRequested) => {
                    Some(NotificationKind::Requested)
                }
                TestEvent::Player(PlayerEvent::HandoverRequested { .. }) => {
                    Some(NotificationKind::HandoverRequested)
                }
                _ => None,
            })
            .collect()
    }

    pub async fn close(self) {
        let Self {
            events,
            host,
            slot,
            player_control,
            worker,
        } = self;
        drop(events);
        drop(slot);
        drop(player_control);
        drop(worker);
        host.close().await;
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

    /// Moves a pending player into the Host; returns whether the player is
    /// resident here rather than transferred.
    async fn ensure_player_inserted(&self) -> bool {
        let previous = std::mem::replace(&mut *self.slot.lock(), PlayerSlot::Resident);
        match previous {
            PlayerSlot::Pending(player) => {
                self.host
                    .insert(*player)
                    .await
                    .unwrap_or_else(|error| panic!("insert offline player into Host: {error}"));
                true
            }
            PlayerSlot::Resident => true,
            PlayerSlot::Transferred => {
                *self.slot.lock() = PlayerSlot::Transferred;
                false
            }
        }
    }
}

/// Test observation tags retained for scenario assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    HandoverRequested,
    PlaybackStarted,
    PlaybackStopped,
    Requested,
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
