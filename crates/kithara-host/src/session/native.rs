use std::num::NonZeroU32;

use firewheel::{FirewheelCtx, backend::AudioBackend};
use kithara_audio::ConsumerWakeMode;
use kithara_bufpool::HasPool;
use kithara_platform::{
    sync::{Arc, Mutex, mpsc},
    thread::spawn_named,
};
use kithara_play::{GroupState, StreamShape, player::PlayerMember};
use kithara_test_utils::kithara;
use tracing::{debug, warn};

use super::{
    dispatch::run_host_cmd,
    protocol::{
        Cmd, HostCmd, HostCmdMsg, HostDispatchError, HostDispatcher, HostReply, Reply,
        SessionDispatcher,
    },
    state::{RootView, SessionState},
};
use crate::error::PlayError;

pub(crate) struct SessionClient<S> {
    cmd_tx: Mutex<mpsc::Sender<HostCmdMsg<S>>>,
}

impl<S> SessionClient<S> {
    /// `no_block`: sync command-reply bridge to the dedicated session thread for host/FFI dispatch.
    #[kithara::allow_block]
    fn call(&self, cmd: HostCmd<S>) -> Result<HostReply, HostDispatchError<S>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        if let Err(error) = self.cmd_tx.lock().send(HostCmdMsg { cmd, reply_tx }) {
            return Err(HostDispatchError::before_send(
                PlayError::SessionGone {
                    reason: "session thread stopped accepting commands",
                },
                error.0.cmd,
            ));
        }
        let reply = reply_rx.recv().map_err(|_| {
            HostDispatchError::after_send(PlayError::SessionGone {
                reason: "session thread dropped the reply channel",
            })
        })?;
        Ok(reply)
    }
}

impl<S: Send + Sync + 'static> SessionDispatcher<S> for SessionClient<S> {
    fn exec(&self, cmd: Cmd<S>) -> Result<Reply, PlayError> {
        match self.call(HostCmd::Play(cmd)).map_err(PlayError::from)? {
            HostReply::Play(reply) => Ok(reply),
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected host reply for player session command".into(),
            )),
        }
    }

    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }
}

impl<S: Send + Sync + 'static> HostDispatcher<S> for SessionClient<S> {
    fn exec_host(&self, cmd: HostCmd<S>) -> Result<HostReply, HostDispatchError<S>> {
        self.call(cmd)
    }
}

fn complete_shutdown<B: AudioBackend, S>(
    cmd_rx: mpsc::Receiver<HostCmdMsg<S>>,
    state: SessionState<B, S>,
    reply_tx: &mpsc::Sender<HostReply>,
) {
    // Disconnect queued callers before PlayerRuntime::drop takes its
    // admission gate; otherwise each side can wait on the other.
    drop(cmd_rx);
    drop(state);
    if reply_tx.send(HostReply::Ok).is_err() {
        warn!("[KITHARA-ROUTE] native shutdown reply receiver dropped");
    }
}

fn engine_thread<B: AudioBackend, S>(
    cmd_rx: mpsc::Receiver<HostCmdMsg<S>>,
    root: GroupState<PlayerMember>,
    root_view: RootView,
    requested_shape: StreamShape,
    start_stream_fn: impl FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send + 'static,
) where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let mut state = SessionState::<B, S>::new(root, root_view, requested_shape, start_stream_fn);
    debug!("[KITHARA-ROUTE] native session worker started");
    while let Ok(HostCmdMsg { cmd, reply_tx }) = cmd_rx.recv() {
        if matches!(&cmd, HostCmd::Shutdown) {
            complete_shutdown(cmd_rx, state, &reply_tx);
            debug!("[KITHARA-ROUTE] native session worker stopped");
            return;
        }
        let reply = run_host_cmd(&mut state, cmd);
        if reply_tx.send(reply).is_err() {
            warn!("[KITHARA-ROUTE] native session reply receiver dropped");
        }
    }
    debug!("[KITHARA-ROUTE] native session worker stopped");
}

fn spawn_session_client<B, S>(
    thread_name: &'static str,
    root: GroupState<PlayerMember>,
    root_view: RootView,
    requested_shape: StreamShape,
    start_stream_fn: impl FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send + 'static,
) -> Arc<SessionClient<S>>
where
    B: AudioBackend + Send + 'static,
    S: HasPool<f32> + Send + Sync + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::channel::<HostCmdMsg<S>>();
    spawn_named(thread_name, move || {
        engine_thread::<B, S>(cmd_rx, root, root_view, requested_shape, start_stream_fn);
    });
    Arc::new(SessionClient {
        cmd_tx: Mutex::new(cmd_tx),
    })
}

fn start_stream_cpal(
    ctx: &mut FirewheelCtx<firewheel::cpal::CpalBackend>,
    sample_rate: u32,
    output_block_frames: NonZeroU32,
) -> Result<(), String> {
    debug!(sample_rate, "[KITHARA-ROUTE] starting cpal stream");
    let config = firewheel::cpal::CpalConfig {
        output: firewheel::cpal::CpalOutputConfig {
            desired_sample_rate: NonZeroU32::new(sample_rate).map(NonZeroU32::get),
            desired_block_frames: Some(output_block_frames.get()),
            ..Default::default()
        },
        ..Default::default()
    };
    match ctx.start_stream(config) {
        Ok(()) => {
            debug!(sample_rate, "[KITHARA-ROUTE] cpal stream started");
            Ok(())
        }
        Err(err) => {
            warn!(
                sample_rate,
                ?err,
                "[KITHARA-ROUTE] cpal stream start failed"
            );
            Err(err.to_string())
        }
    }
}

pub(crate) fn spawn<S: HasPool<f32> + Send + Sync + 'static>(
    root: GroupState<PlayerMember>,
    root_view: RootView,
    requested_shape: StreamShape,
) -> Arc<dyn HostDispatcher<S>> {
    let output_block_frames = requested_shape.max_block_frames;
    spawn_session_client::<firewheel::cpal::CpalBackend, S>(
        "kithara-engine",
        root,
        root_view,
        requested_shape,
        move |ctx, sample_rate| start_stream_cpal(ctx, sample_rate, output_block_frames),
    )
}
