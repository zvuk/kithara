//! Per-instance offline harness driving the full PlayerImpl chain.
//!
//! Owns an [`EngineImpl::new_offline`] pair: an engine wired to a
//! private offline session worker, and an [`OfflineSessionHandle`] for
//! synchronously stepping the firewheel graph from the test thread.
//! Auto-advance, pending_next preload, and crossfade activation all
//! exercise the production [`PlayerImpl`] paths — only the audio
//! backend is swapped from cpal to a stub that renders on demand.
//!
//! Use the `render*` family to drive the graph and `tick_and_drain`
//! to convert audio-thread `PlayerNotification`s into observable
//! `PlayerEvent`s.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use kithara_events::{Event, EventReceiver, PlayerEvent};
use kithara_platform::Mutex;
use kithara_play::{
    EngineConfig, EngineImpl, PlayerConfig, PlayerImpl,
    internal::offline::OfflineSessionHandle,
};

pub(crate) struct OfflinePlayerHarness {
    events: Mutex<EventReceiver>,
    player: Arc<PlayerImpl>,
    session: OfflineSessionHandle,
}

impl OfflinePlayerHarness {
    pub(crate) fn with_sample_rate(mut player_config: PlayerConfig, sample_rate: u32) -> Self {
        let bus = player_config.bus.clone().unwrap_or_default();
        player_config.bus = Some(bus.clone());

        let engine_config = EngineConfig {
            eq_layout: player_config.eq_layout.clone(),
            max_slots: player_config.max_slots,
            pcm_pool: player_config.pcm_pool.clone(),
            sample_rate,
            ..EngineConfig::default()
        };
        let (engine, session) = EngineImpl::new_offline(engine_config, bus);
        let player = Arc::new(PlayerImpl::with_engine(player_config, engine));
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
        use kithara_platform::tokio::sync::broadcast::error::TryRecvError;
        self.player.process_notifications();

        let mut events = Vec::new();
        let mut rx = self.events.lock_sync();
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
