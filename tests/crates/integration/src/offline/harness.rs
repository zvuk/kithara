use std::num::{NonZeroU32, NonZeroUsize};

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
    output_block_frames: Option<NonZeroU32>,
    response_budget_frames: Option<NonZeroUsize>,
}

/// Build a paused queue with crossfade disabled for deterministic offline tests.
pub async fn offline_queue_fixture(
    sample_rate: u32,
) -> (OfflinePlayerHarness, QueueControl<TestPools>) {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        sample_rate,
    )
    .await;
    let config = QueueConfig::builder()
        .player(harness.take_player())
        .should_autoplay(false)
        .build();
    let queue = harness.insert_control(Queue::new(config)).await;
    (harness, queue)
}

impl OfflinePlayerHarness {
    pub async fn with_sample_rate(options: OfflinePlayerOptions, sample_rate: u32) -> Self {
        let pools = pools();
        let sample_rate =
            NonZeroU32::new(sample_rate).expect("offline player sample rate must be non-zero");
        let session = HostConfig::offline(pools)
            .sample_rate(sample_rate)
            .maybe_max_block_frames(options.output_block_frames)
            .build();
        Self::new(options, session).await
    }

    pub async fn new(options: OfflinePlayerOptions, session: HostConfig<TestPools>) -> Self {
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
            .maybe_response_budget_frames(options.response_budget_frames)
            .build();

        let player = PlayerImpl::new(player_config);
        let player_control = player.control();
        let events = player.subscribe();
        let host = OfflineHostHarness::new(session)
            .await
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

    /// Issues player control calls from the host owner thread, as the app
    /// would.
    pub async fn with_player<R>(
        &self,
        use_player: impl FnOnce(&PlayerControl<TestPools>) -> R + Send + 'static,
    ) -> R
    where
        R: Send + 'static,
    {
        self.ensure_player_inserted().await;
        self.run(&self.player_control, use_player).await
    }

    /// Issues a control call on `control` from the host owner thread, as the
    /// app would.
    pub async fn run<C, R>(&self, control: &C, f: impl FnOnce(&C) -> R + Send + 'static) -> R
    where
        C: Clone + Send + 'static,
        R: Send + 'static,
    {
        let control = control.clone();
        self.host.run(move || f(&control)).await
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

    pub async fn insert<P>(&self, player: P) -> HostOwned<P>
    where
        P: PlayerControlSource<Schema = TestPools> + Send + 'static,
    {
        self.host
            .insert(player)
            .await
            .unwrap_or_else(|error| panic!("insert player facade into offline Host: {error}"))
    }

    pub async fn insert_control<P>(&self, player: P) -> P::Control
    where
        P: PlayerControlSource<Schema = TestPools> + Send + 'static,
    {
        self.insert(player).await.control().clone()
    }

    pub const fn host(&self) -> &OfflineHostHarness<TestPools> {
        &self.host
    }

    pub async fn close(self) {
        let Self {
            events,
            host,
            player,
            player_control,
            worker,
        } = self;
        drop(events);
        drop(player);
        drop(player_control);
        drop(worker);
        host.close().await;
    }

    /// Render `frames` of audio through the product Host.
    pub async fn render(&self, frames: usize) -> Vec<f32> {
        self.ensure_player_inserted().await;
        self.host.render(frames).await
    }

    async fn ensure_player_inserted(&self) {
        let pending = self.player.lock().take();
        if let Some(player) = pending {
            self.host
                .insert(player)
                .await
                .unwrap_or_else(|error| panic!("insert offline player into Host: {error}"));
        }
    }

    /// Pump the player's notification ringbuf and drain `PlayerEvent`s
    /// from the bus subscriber.
    pub async fn tick_and_drain(&self) -> Vec<PlayerEvent> {
        self.run(&self.player_control, PlayerControl::process_notifications)
            .await;

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
