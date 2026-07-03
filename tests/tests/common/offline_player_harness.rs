#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use kithara::{
    events::{Event, EventReceiver, PlayerEvent},
    platform::sync::Mutex,
    play::{PlayerConfig, PlayerImpl, SessionDispatcher},
};
use kithara_integration_tests::offline::OfflineSession;

pub(crate) struct OfflinePlayerHarness {
    events: Mutex<EventReceiver>,
    player: Arc<PlayerImpl>,
    session: Arc<OfflineSession>,
}

impl OfflinePlayerHarness {
    pub(crate) fn with_sample_rate(mut player_config: PlayerConfig, sample_rate: u32) -> Self {
        let session = Arc::new(OfflineSession::new_manual());
        player_config.sample_rate = sample_rate;
        player_config.session = Some(Arc::clone(&session) as Arc<dyn SessionDispatcher>);

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
            match rx.try_recv() {
                Ok(Event::Player(event)) => events.push(event),
                Ok(_) => continue,
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        events
    }
}
