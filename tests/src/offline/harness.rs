use std::num::NonZeroU32;

use kithara::{
    decode::GaplessMode,
    events::{Event, EventReceiver, PlayerEvent},
    host::{HostConfig, HostOwned},
    platform::{sync::Mutex, tokio::sync::broadcast::error::TryRecvError},
    play::{
        PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl,
        effects::eq::EqBandConfig,
        player::{Player, PlayerControl, PlayerControlSource},
    },
    queue::{Queue, QueueConfig, QueueControl},
    warp::WarpConfig,
};

use super::{OfflineHostHarness, host::offline_pools};
use crate::bufpool_ext::{TestPools, pools};

pub struct OfflinePlayerHarness {
    events: Mutex<EventReceiver>,
    host: OfflineHostHarness<TestPools>,
    player: Mutex<Option<PlayerImpl<TestPools>>>,
    player_control: PlayerControl<TestPools>,
    worker: PlayWorker<TestPools>,
}

#[derive(Clone, bon::Builder)]
pub struct OfflinePlayerOptions {
    #[builder(default = 1.0)]
    crossfade_duration: f32,
    eq_layout: Option<Vec<EqBandConfig>>,
    #[builder(default)]
    gapless_mode: GaplessMode,
    /// Make audio-thread reads block on a producer-ring underrun instead of
    /// zero-filling. Only suites that measure absolute rendered length
    /// (gapless) opt in: blocking trades an underrun for waiting on decode,
    /// so under a tight `hang_timeout_secs` a slow decode becomes a hang
    /// panic instead of inserted silence.
    #[builder(default)]
    block_on_underrun: bool,
    warp: Option<WarpConfig>,
}

/// Build a paused queue with crossfade disabled for deterministic offline tests.
#[must_use]
pub fn offline_queue_fixture(sample_rate: u32) -> (OfflinePlayerHarness, QueueControl<TestPools>) {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        sample_rate,
    );
    let config = QueueConfig::builder()
        .player(harness.take_player())
        .should_autoplay(false)
        .build();
    let queue = harness.insert_control(Queue::new(config));
    (harness, queue)
}

impl OfflinePlayerHarness {
    pub fn with_sample_rate(options: OfflinePlayerOptions, sample_rate: u32) -> Self {
        let pools = pools();
        let sample_rate =
            NonZeroU32::new(sample_rate).expect("offline player sample rate must be non-zero");
        let session = HostConfig::offline(pools).sample_rate(sample_rate).build();
        Self::new(options, session)
    }

    pub fn new(options: OfflinePlayerOptions, session: HostConfig<TestPools>) -> Self {
        let sample_rate = session.sample_rate();
        let pools = offline_pools(&session).clone();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
        let player_config = PlayerConfig::builder()
            .crossfade_duration(options.crossfade_duration)
            .gapless_mode(options.gapless_mode)
            .block_on_underrun(options.block_on_underrun)
            .sample_rate(sample_rate)
            .worker(worker.clone())
            .maybe_eq_layout(options.eq_layout)
            .maybe_warp(options.warp)
            .build();

        let player = PlayerImpl::new(player_config);
        let player_control = player.control();
        let events = player.subscribe();
        let host = OfflineHostHarness::new(session)
            .unwrap_or_else(|error| panic!("create product offline Host: {error}"));

        Self {
            events: Mutex::new(events),
            host,
            player: Mutex::new(Some(player)),
            player_control,
            worker,
        }
    }

    pub const fn player(&self) -> &PlayerControl<TestPools> {
        &self.player_control
    }

    pub fn take_player(&self) -> PlayerImpl<TestPools> {
        self.player
            .lock()
            .take()
            .expect("offline harness player was already transferred")
    }

    pub fn with_player<R>(&self, use_player: impl FnOnce(&PlayerControl<TestPools>) -> R) -> R {
        self.ensure_player_inserted();
        use_player(&self.player_control)
    }

    pub const fn worker(&self) -> &PlayWorker<TestPools> {
        &self.worker
    }

    pub fn set_host_level(&self, level: f32) {
        self.player
            .lock()
            .as_ref()
            .expect("offline harness player was already transferred")
            .set_host_level(level);
    }

    pub fn insert<P>(&self, player: P) -> HostOwned<P>
    where
        P: PlayerControlSource<Schema = TestPools>,
    {
        self.host
            .insert(player)
            .unwrap_or_else(|error| panic!("insert player facade into offline Host: {error}"))
    }

    pub fn insert_control<P>(&self, player: P) -> P::Control
    where
        P: PlayerControlSource<Schema = TestPools>,
    {
        self.insert(player).control().clone()
    }

    pub const fn host(&self) -> &OfflineHostHarness<TestPools> {
        &self.host
    }

    /// Synchronously render `frames` of audio.
    pub fn render(&self, frames: usize) -> Vec<f32> {
        self.ensure_player_inserted();
        self.host.render(frames)
    }

    fn ensure_player_inserted(&self) {
        let mut player = self.player.lock();
        if let Some(player) = player.take() {
            self.host
                .insert(player)
                .unwrap_or_else(|error| panic!("insert offline player into Host: {error}"));
        }
    }

    /// Pump the player's notification ringbuf and drain `PlayerEvent`s
    /// from the bus subscriber.
    pub fn tick_and_drain(&self) -> Vec<PlayerEvent> {
        self.player_control.process_notifications();

        let mut events = Vec::new();
        let mut rx = self.events.lock();
        loop {
            match rx.try_recv().map(|env| env.event) {
                Ok(Event::Player(event)) => events.push(event),
                Ok(_) | Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        events
    }
}
