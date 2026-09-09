//! The lifecycle contract runs through the same Host graph with a cpal backend.
//! A test-only dispatcher owns that graph so the production Host never exposes
//! its resident engine or raw session.
use firewheel::{FirewheelCtx, cpal::CpalBackend};
use kithara::{
    audio::ConsumerWakeMode,
    host::testing::GraphSession,
    platform::{
        sync::{Arc, Mutex, mpsc},
        thread::{JoinHandle, spawn_named},
    },
    play::{
        Cmd, EngineImpl, PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, Reply,
        SessionBinding, SessionDispatcher, player::Player,
    },
};
use kithara_integration_tests::test_defaults::Consts as Shared;

use super::engine_session_contract as contract;
use crate::bufpool_ext::{TestPools, pools};

struct CpalGraphSession {
    cmd_tx: Mutex<mpsc::Sender<CpalMessage>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

enum CpalMessage {
    Command {
        cmd: Cmd<TestPools>,
        reply_tx: mpsc::Sender<Reply>,
    },
    Shutdown,
}

impl CpalGraphSession {
    fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<CpalMessage>();
        let worker = spawn_named("kithara-engine-cpal-contract", move || {
            let mut graph = GraphSession::<CpalBackend, TestPools>::new(start_stream);
            while let Ok(message) = cmd_rx.recv() {
                match message {
                    CpalMessage::Command { cmd, reply_tx } => {
                        let _ = reply_tx.send(graph.exec(cmd));
                    }
                    CpalMessage::Shutdown => break,
                }
            }
        });
        Self {
            cmd_tx: Mutex::new(cmd_tx),
            worker: Mutex::new(Some(worker)),
        }
    }
}

impl Drop for CpalGraphSession {
    fn drop(&mut self) {
        let _ = self.cmd_tx.lock().send(CpalMessage::Shutdown);
        if let Some(worker) = self.worker.lock().take() {
            let _ = worker.join();
        }
    }
}

impl SessionDispatcher<TestPools> for CpalGraphSession {
    #[kithara::allow_block]
    fn exec(&self, cmd: Cmd<TestPools>) -> Result<Reply, PlayError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .lock()
            .send(CpalMessage::Command { cmd, reply_tx })
            .map_err(|_| PlayError::SessionGone {
                reason: "cpal contract session stopped accepting commands",
            })?;
        reply_rx.recv().map_err(|_| PlayError::SessionGone {
            reason: "cpal contract session dropped its reply channel",
        })
    }

    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }
}

fn start_stream(ctx: &mut FirewheelCtx<CpalBackend>, sample_rate: u32) -> Result<(), String> {
    ctx.start_stream(firewheel::cpal::CpalConfig {
        output: firewheel::cpal::CpalOutputConfig {
            desired_sample_rate: Some(sample_rate),
            ..Default::default()
        },
        ..Default::default()
    })
    .map_err(|error| error.to_string())
}

fn run_contract(max_slots: usize, contract: impl FnOnce(&EngineImpl<TestPools>)) {
    let session: Arc<dyn SessionDispatcher<TestPools>> = Arc::new(CpalGraphSession::new());
    let mut player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .max_slots(max_slots)
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .session(SessionBinding::new(session, Shared::NON_ZERO_SAMPLE_RATE))
            .build(),
    );
    contract(player.engine());
    Player::close(&mut player).expect("close cpal fixture player");
}

#[kithara::test]
fn engine_start_stop_roundtrip() {
    run_contract(4, contract::start_stop_roundtrip);
}

#[kithara::test]
fn engine_allocate_and_release_slot() {
    run_contract(4, contract::allocate_and_release_slot);
}

#[kithara::test]
fn engine_arena_full_error() {
    run_contract(1, contract::arena_full_error);
}
