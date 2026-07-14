#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    audio::{EqBandConfig, StretchControls},
    bufpool::Region,
    decode::GaplessMode,
    events::{Event, EventReceiver, PlayerEvent},
    platform::sync::{Arc, Mutex},
    play::{PlayerConfig, PlayerImpl, SessionDispatcher},
};
use kithara_integration_tests::offline::OfflineSession;

pub(crate) struct OfflinePlayerHarness {
    events: Mutex<EventReceiver>,
    player: Arc<PlayerImpl>,
    session: Arc<OfflineSession>,
}

#[derive(Clone, bon::Builder)]
pub(crate) struct OfflinePlayerOptions {
    #[builder(default = 1.0)]
    crossfade_duration: f32,
    eq_layout: Option<Vec<EqBandConfig>>,
    #[builder(default)]
    gapless_mode: GaplessMode,
    timestretch: Option<Arc<StretchControls>>,
}

impl OfflinePlayerHarness {
    pub(crate) fn with_sample_rate(options: OfflinePlayerOptions, sample_rate: u32) -> Self {
        let session = Arc::new(OfflineSession::new_manual());
        let session_dispatcher = Arc::clone(&session) as Arc<dyn SessionDispatcher>;
        let region = Region::default();
        let player_config = PlayerConfig::builder()
            .crossfade_duration(options.crossfade_duration)
            .gapless_mode(options.gapless_mode)
            .sample_rate(sample_rate)
            .session(Arc::clone(&session_dispatcher))
            .byte_pool(region.byte_pool())
            .pcm_pool(region.pcm_pool())
            .maybe_eq_layout(options.eq_layout)
            .maybe_timestretch(options.timestretch)
            .build();

        let player = Arc::new(PlayerImpl::new(player_config));
        let events = player.subscribe();

        Self {
            events: Mutex::new(events),
            player,
            session,
        }
    }

    pub(crate) fn player(&self) -> &Arc<PlayerImpl> {
        &self.player
    }

    /// Synchronously render `frames` of audio.
    pub(crate) fn render(&self, frames: usize) -> Vec<f32> {
        self.session.render(frames)
    }

    /// Pump the player's notification ringbuf and drain `PlayerEvent`s
    /// from the bus subscriber.
    pub(crate) fn tick_and_drain(&self) -> Vec<PlayerEvent> {
        use kithara::platform::tokio::sync::broadcast::error::TryRecvError;
        self.player.process_notifications();

        let mut events = Vec::new();
        let mut rx = self.events.lock();
        loop {
            match rx.try_recv().map(|env| env.event) {
                Ok(Event::Player(event)) => events.push(event),
                Ok(_) => continue,
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        events
    }
}
