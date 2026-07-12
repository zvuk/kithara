#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    audio::{EqBandConfig, StretchControls},
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

#[derive(Clone)]
pub(crate) struct OfflinePlayerOptions {
    crossfade_duration: f32,
    eq_layout: Option<Vec<EqBandConfig>>,
    gapless_mode: GaplessMode,
    timestretch: Option<Arc<StretchControls>>,
}

impl Default for OfflinePlayerOptions {
    fn default() -> Self {
        Self {
            crossfade_duration: 1.0,
            eq_layout: None,
            gapless_mode: GaplessMode::default(),
            timestretch: None,
        }
    }
}

impl OfflinePlayerOptions {
    #[must_use]
    pub(crate) fn crossfade_duration(mut self, duration: f32) -> Self {
        self.crossfade_duration = duration;
        self
    }

    #[must_use]
    pub(crate) fn eq_layout(mut self, layout: Vec<EqBandConfig>) -> Self {
        self.eq_layout = Some(layout);
        self
    }

    #[must_use]
    pub(crate) fn gapless_mode(mut self, mode: GaplessMode) -> Self {
        self.gapless_mode = mode;
        self
    }

    #[must_use]
    pub(crate) fn timestretch(mut self, controls: Arc<StretchControls>) -> Self {
        self.timestretch = Some(controls);
        self
    }
}

impl OfflinePlayerHarness {
    pub(crate) fn with_sample_rate(options: OfflinePlayerOptions, sample_rate: u32) -> Self {
        let session = Arc::new(OfflineSession::new_manual());
        let session_dispatcher = Arc::clone(&session) as Arc<dyn SessionDispatcher>;
        let builder = || {
            PlayerConfig::builder()
                .crossfade_duration(options.crossfade_duration)
                .gapless_mode(options.gapless_mode)
                .sample_rate(sample_rate)
                .session(Arc::clone(&session_dispatcher))
        };
        let player_config = match (options.eq_layout, options.timestretch) {
            (Some(layout), Some(controls)) => {
                builder().eq_layout(layout).timestretch(controls).build()
            }
            (Some(layout), None) => builder().eq_layout(layout).build(),
            (None, Some(controls)) => builder().timestretch(controls).build(),
            (None, None) => builder().build(),
        };

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
