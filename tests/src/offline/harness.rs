use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    decode::GaplessMode,
    events::{Event, EventReceiver, PlayerEvent},
    platform::{
        sync::{Arc, Mutex},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{
        PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, SessionDispatcher,
        effects::eq::EqBandConfig,
        player::{PlayerControl, PlayerControlSource},
    },
    warp::WarpConfig,
};

use super::OfflineSession;
use crate::bufpool_ext::{TestPools, pools};

pub struct OfflinePlayerHarness {
    events: Mutex<EventReceiver>,
    player: Mutex<Option<PlayerImpl<TestPools>>>,
    player_control: PlayerControl<TestPools>,
    session: Arc<OfflineSession>,
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

impl OfflinePlayerHarness {
    pub fn with_sample_rate(options: OfflinePlayerOptions, sample_rate: u32) -> Self {
        let session = Arc::new(match options.output_block_frames {
            Some(output_block_frames) => OfflineSession::new_manual_with_block_frames(
                usize::try_from(output_block_frames.get()).expect("offline block size fits usize"),
            ),
            None => OfflineSession::new_manual(),
        });
        let session_dispatcher = Arc::clone(&session) as Arc<dyn SessionDispatcher<TestPools>>;
        let pools = pools();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
        let player_config = PlayerConfig::builder()
            .crossfade_duration(options.crossfade_duration)
            .gapless_mode(options.gapless_mode)
            .block_on_underrun(options.block_on_underrun)
            .sample_rate(
                NonZeroU32::new(sample_rate).expect("offline player sample rate must be non-zero"),
            )
            .session(Arc::clone(&session_dispatcher))
            .worker(worker)
            .maybe_eq_layout(options.eq_layout)
            .maybe_warp(options.warp)
            .maybe_response_budget_frames(options.response_budget_frames)
            .build();

        let player = PlayerImpl::new(player_config);
        let player_control = player.control();
        let events = player.subscribe();

        Self {
            events: Mutex::new(events),
            player: Mutex::new(Some(player)),
            player_control,
            session,
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

    pub fn with_player<R>(&self, use_player: impl FnOnce(&PlayerImpl<TestPools>) -> R) -> R {
        let player = self.player.lock();
        use_player(
            player
                .as_ref()
                .expect("offline harness player was already transferred"),
        )
    }

    pub fn session(&self) -> &Arc<OfflineSession> {
        &self.session
    }

    /// Synchronously render `frames` of audio.
    pub fn render(&self, frames: usize) -> Vec<f32> {
        self.session.render(frames)
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
