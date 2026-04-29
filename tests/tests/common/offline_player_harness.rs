#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use kithara_events::{Event, EventReceiver};
use kithara_platform::Mutex;
use kithara_play::{
    EngineConfig, EngineImpl, PlayerConfig, PlayerEvent, PlayerImpl,
    impls::offline_backend::OfflineConfig,
};

pub(crate) struct OfflinePlayerHarness {
    events: Mutex<EventReceiver>,
    player: Arc<PlayerImpl>,
    render_block_frames: usize,
}

impl OfflinePlayerHarness {
    pub(crate) fn new(mut player_config: PlayerConfig, offline: OfflineConfig) -> Self {
        let bus = player_config.bus.clone().unwrap_or_default();
        player_config.bus = Some(bus.clone());

        let engine_config = EngineConfig {
            eq_layout: player_config.eq_layout.clone(),
            max_slots: player_config.max_slots,
            pcm_pool: player_config.pcm_pool.clone(),
            ..EngineConfig::default()
        };
        let engine = EngineImpl::new_offline(engine_config, bus, offline.clone());
        let player = Arc::new(PlayerImpl::with_engine(player_config, engine));
        let events = player.subscribe();

        Self {
            events: Mutex::new(events),
            player,
            render_block_frames: offline.block_frames as usize,
        }
    }

    pub(crate) fn player(&self) -> &Arc<PlayerImpl> {
        &self.player
    }

    pub(crate) fn render(&self, frames: usize) -> Vec<f32> {
        self.player
            .engine()
            .render_offline(frames)
            .expect("render offline player harness")
    }

    pub(crate) fn render_block(&self) -> Vec<f32> {
        self.render(self.render_block_frames)
    }

    pub(crate) fn render_block_and_drain(&self) -> (Vec<f32>, Vec<PlayerEvent>) {
        let block = self.render_block();
        let events = self.tick_and_drain();
        (block, events)
    }

    pub(crate) fn render_until<P: FnMut(&[f32], &[PlayerEvent]) -> bool>(
        &self,
        mut predicate: P,
        max_frames: usize,
    ) -> Vec<f32> {
        let mut rendered = Vec::new();
        let mut drained_events = Vec::new();
        let mut frames_rendered = 0usize;

        while frames_rendered < max_frames {
            let frames = self
                .render_block_frames
                .min(max_frames.saturating_sub(frames_rendered));
            if frames == 0 {
                break;
            }

            rendered.extend(self.render(frames));
            frames_rendered += frames;
            drained_events.extend(self.tick_and_drain());

            if predicate(&rendered, &drained_events) {
                break;
            }
        }

        rendered
    }

    pub(crate) fn tick_and_drain(&self) -> Vec<PlayerEvent> {
        self.player.process_notifications();

        let mut events = Vec::new();
        let mut rx = self.events.lock_sync();
        while let Ok(event) = rx.try_recv() {
            if let Event::Player(event) = event {
                events.push(event);
            }
        }
        events
    }
}
