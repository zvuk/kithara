use kithara::{
    audio::AudioReader,
    events::{Event, EventReceiver, PlayerEvent},
    host::HostConfig,
    platform::sync::Arc,
    play::{
        PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, Resource,
        bridge::RtMetricsSnapshot, player::PlayerControl,
    },
};

use super::{OfflineHostHarness, OfflineResident, host::offline_pools};
use crate::bufpool_ext::TestPools;

/// Product Player and Host wired for deterministic finite rendering.
pub struct OfflinePlayer {
    events: EventReceiver,
    player: OfflineResident<PlayerImpl<TestPools>, TestPools>,
    worker: PlayWorker<TestPools>,
}

impl OfflinePlayer {
    /// Create an offline player from the product session configuration.
    ///
    /// # Panics
    ///
    /// Panics if the product offline Host cannot be initialised.
    #[must_use]
    pub async fn new(session: HostConfig<TestPools>) -> Self {
        let sample_rate = session.sample_rate();
        let pools = offline_pools(&session).clone();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(sample_rate)
                .worker(worker.clone())
                .build(),
        );
        let events = player.subscribe();
        let player = OfflineResident::new(session, player)
            .await
            .unwrap_or_else(|error| panic!("create product offline player: {error}"));
        Self {
            events,
            player,
            worker,
        }
    }

    fn control(&self) -> PlayerControl<TestPools> {
        self.player.control()
    }

    /// Offline Host owning this player, for probes the Host installs.
    #[must_use]
    pub const fn host(&self) -> &OfflineHostHarness<TestPools> {
        self.player.host()
    }

    /// Decode worker this player pulls from, for opening resources beside it.
    #[must_use]
    pub const fn worker(&self) -> &PlayWorker<TestPools> {
        &self.worker
    }

    /// Load one resource and start playback.
    ///
    /// # Panics
    ///
    /// Panics if the product player rejects the resource.
    pub async fn load_and_fadein(&mut self, resource: Resource) {
        self.player
            .run(move |control| {
                control.reserve_slots(1);
                control
                    .replace_item(0, resource, kithara::events::TrackId::allocate())
                    .expect("replace offline player item");
                control.play();
            })
            .await;
    }

    /// Set the transition duration used by the next load.
    pub fn set_fade_duration(&mut self, seconds: f32) {
        self.control().set_crossfade_duration(seconds);
    }

    /// Snapshot the real-time counters owned by this player slot.
    #[must_use]
    pub fn metrics(&self) -> RtMetricsSnapshot {
        self.control().rt_metrics().unwrap_or_default()
    }

    /// Current playback position in seconds.
    #[must_use]
    pub fn position(&self) -> f64 {
        self.control().position_seconds().unwrap_or_default()
    }

    /// Render `frames` of interleaved stereo audio through the product Host.
    pub async fn render(&mut self, frames: usize) -> Vec<f32> {
        let output = self.player.render(frames).await;
        self.control().process_notifications();
        output
    }

    /// Seek through the product player. The product runtime owns seek epochs.
    pub fn seek(&mut self, seconds: f64, _seek_epoch: u64) {
        self.control()
            .seek_seconds(seconds)
            .unwrap_or_else(|error| panic!("seek offline player: {error}"));
    }

    /// Drain the product event stream into the scenario observation tags.
    pub fn take_notification_kinds(&mut self) -> Vec<NotificationKind> {
        self.control().process_notifications();
        let mut notifications = Vec::new();
        while let Ok(envelope) = self.events.try_recv() {
            let kind = match envelope.event {
                Event::Player(PlayerEvent::PlaybackStarted { .. }) => {
                    Some(NotificationKind::PlaybackStarted)
                }
                Event::Player(PlayerEvent::ItemDidPlayToEnd { .. })
                | Event::Player(PlayerEvent::ItemDidFail { .. }) => {
                    Some(NotificationKind::PlaybackStopped)
                }
                Event::Player(PlayerEvent::PrefetchRequested) => Some(NotificationKind::Requested),
                Event::Player(PlayerEvent::HandoverRequested { .. }) => {
                    Some(NotificationKind::HandoverRequested)
                }
                _ => None,
            };
            notifications.extend(kind);
        }
        notifications
    }

    pub async fn close(self) {
        let Self {
            events,
            player,
            worker,
        } = self;
        drop(events);
        drop(worker);
        player.close().await;
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
