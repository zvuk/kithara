use std::ops::Deref;

use delegate::delegate;
use kithara_abr::{AbrController, AbrSettings};
use kithara_bufpool::HasPool;
use kithara_platform::{
    CancelScope,
    sync::{Arc, Mutex},
};
use kithara_warp::{SessionEpoch, SyncMemberKind};

use super::{PlayerCore, PlayerLifecycle, PlayerRuntime};
use crate::{
    engine::{EngineConfig, EngineImpl},
    error::PlayError,
    player::{
        PlayerConfig, PlayerControl,
        protocol::PlayerSync,
        state::{ItemQueue, PlayerParams, PlayerPhase},
    },
    worker::EngineLoad,
};

/// Concrete Player implementation managing items queue.
pub struct PlayerImpl<S> {
    pub(crate) runtime: Arc<PlayerRuntime<S>>,
    pub(crate) sync: PlayerSync,
}

impl<S> Deref for PlayerImpl<S> {
    type Target = PlayerRuntime<S>;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

impl<S> PlayerImpl<S> {
    pub(in crate::player) fn make_control(&self) -> PlayerControl<S>
    where
        S: HasPool<f32>,
    {
        PlayerControl::new(Arc::clone(&self.runtime))
    }

    /// Create a new player with the given configuration.
    #[must_use]
    pub fn new(mut config: PlayerConfig<S>) -> Self {
        let pools = config.worker.pools().clone();
        let sync = PlayerSync::unavailable(
            config.grid_id,
            config.sample_rate,
            SessionEpoch::new(0),
            SyncMemberKind::Grid,
        );

        let bus = config.bus.clone().unwrap_or_default();

        // Composed/standalone seam: `Some(parent)` → the player's master is a
        // child of it (so a passed cancel reaches the player but the player's
        // Drop never cancels the passed token); `None` → own root.
        let cancel = CancelScope::new(config.cancel.clone()).token();
        config.cancel = Some(cancel.clone());

        let engine_config = EngineConfig::builder()
            .eq_layout(config.eq_layout.clone())
            .grid_id(config.grid_id)
            .max_slots(config.max_slots)
            .sample_rate(config.sample_rate.get())
            .response_budget_frames(config.response_budget_frames)
            .render_quantum_frames(config.warp.render_quantum_frames())
            .pools(pools)
            .maybe_session(config.session.clone())
            .cancel(cancel.clone())
            .build();
        let engine = EngineImpl::new(engine_config, bus.clone());
        if config.abr.is_none() {
            let abr_settings = AbrSettings::builder().cancel(cancel.clone()).build();
            config.abr = Some(AbrController::new(abr_settings));
        }

        // Seed the single speed source with the configured default rate.
        config.warp.stretch().set_speed(config.default_rate);
        let params = PlayerParams::from(&config);
        let core = PlayerCore {
            engine,
            worker: config.worker,
            engine_load: Arc::new(EngineLoad::default()),
            params,
            warp: config.warp,
            response_budget_frames: config.response_budget_frames,
            gapless_mode: config.gapless_mode,
            block_on_underrun: config.block_on_underrun,
            status: Mutex::default(),
            items: ItemQueue::new(bus),
        };
        Self {
            runtime: Arc::new(PlayerRuntime {
                lifecycle: PlayerLifecycle::open(),
                operations: Mutex::default(),
                core,
                phase: Mutex::new(PlayerPhase::Idle),
            }),
            sync,
        }
    }
}

impl<S> Drop for PlayerImpl<S> {
    fn drop(&mut self) {
        self.runtime.invalidate();
    }
}

impl<S> crate::api::Equalizer for PlayerImpl<S>
where
    S: Send + Sync + 'static,
{
    delegate! {
        to self {
            #[call(eq_band_count)]
            fn band_count(&self) -> usize;
            #[call(eq_gain)]
            fn gain(&self, band: usize) -> Option<f32>;
            #[call(reset_eq)]
            fn reset(&self) -> Result<(), PlayError>;
            #[call(set_eq_gain)]
            fn set_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError>;
        }
    }
}
