use std::num::NonZeroU32;

use firewheel::{
    FirewheelContext,
    cpal::{CpalConfig, CpalStream},
};
use kithara_audio::ConsumerWakeMode;
use kithara_bufpool::HasPool;
use kithara_effects::LimiterConfig;
use kithara_platform::{
    sync::{Arc, Mutex, mpsc, mpsc::RecvTimeoutError},
    thread::spawn_named,
    time::{Duration, Instant},
};
use kithara_play::{SessionSampleRate, StreamShape};
use kithara_sync::GroupState;
use tracing::{debug, warn};

use super::{
    dispatch::run_host_cmd,
    protocol::{
        Cmd, HostCmd, HostCmdMsg, HostDispatchError, HostDispatcher, HostReply, Reply,
        SessionDispatcher,
    },
    state::{RootView, SessionState},
};
use crate::{PlayerMember, error::PlayError};

pub(crate) struct SessionClient<S> {
    cmd_tx: Mutex<mpsc::Sender<HostCmdMsg<S>>>,
    root_view: RootView,
}

impl<S> SessionClient<S> {
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
    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }

    fn exec(&self, cmd: Cmd<S>) -> Result<Reply, PlayError> {
        match self.call(HostCmd::Play(cmd)).map_err(PlayError::from)? {
            HostReply::Play(reply) => Ok(reply),
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected host reply for player session command".into(),
            )),
        }
    }

    delegate::delegate! {
        to self.root_view {
            #[expr(Ok($))]
            fn sample_rate(&self) -> Result<SessionSampleRate, PlayError>;
            #[expr(Ok($))]
            fn stream_shape(&self) -> Result<Option<StreamShape>, PlayError>;
        }
    }
}

impl<S: Send + Sync + 'static> HostDispatcher<S> for SessionClient<S> {
    fn exec_host(&self, cmd: HostCmd<S>) -> Result<HostReply, HostDispatchError<S>> {
        self.call(cmd)
    }
}

/// Disconnects queued callers before `PlayerRuntime::drop` takes its admission gate, since
/// otherwise each side can wait on the other.
fn complete_shutdown<T, S>(
    cmd_rx: mpsc::Receiver<HostCmdMsg<S>>,
    state: SessionState<T, S>,
    reply_tx: &mpsc::Sender<HostReply>,
) {
    drop(cmd_rx);
    drop(state);
    if reply_tx.send(HostReply::Ok).is_err() {
        warn!("[KITHARA-ROUTE] native shutdown reply receiver dropped");
    }
}

fn idle_tick<T, S>(state: &mut SessionState<T, S>)
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    if let HostReply::Play(Reply::Err(error)) = run_host_cmd(state, HostCmd::Play(Cmd::Tick))
        && !matches!(error, crate::session::SessionError::SyncControlBusy)
    {
        warn!(?error, "native session tick failed");
    }
}

fn engine_thread<T, S>(
    cmd_rx: mpsc::Receiver<HostCmdMsg<S>>,
    root: GroupState<PlayerMember>,
    root_view: RootView,
    sample_rate: NonZeroU32,
    requested_max_block_frames: Option<NonZeroU32>,
    limiter: LimiterConfig,
    start_stream_fn: impl FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static,
) where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let mut state = SessionState::<T, S>::new(
        root,
        root_view,
        sample_rate,
        requested_max_block_frames,
        None,
        limiter,
        start_stream_fn,
    );
    debug!("[KITHARA-ROUTE] native session worker started");
    loop {
        let message = match cmd_rx.recv_timeout(Instant::now() + Duration::from_millis(2)) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => {
                idle_tick(&mut state);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let HostCmdMsg { cmd, reply_tx } = message;
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

fn spawn_session_client<T, S>(
    thread_name: &'static str,
    root: GroupState<PlayerMember>,
    root_view: RootView,
    sample_rate: NonZeroU32,
    requested_max_block_frames: Option<NonZeroU32>,
    limiter: LimiterConfig,
    start_stream_fn: impl FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static,
) -> Arc<SessionClient<S>>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::channel::<HostCmdMsg<S>>();
    let client_view = root_view.clone();
    spawn_named(thread_name, move || {
        engine_thread::<T, S>(
            cmd_rx,
            root,
            root_view,
            sample_rate,
            requested_max_block_frames,
            limiter,
            start_stream_fn,
        );
    });
    Arc::new(SessionClient {
        root_view: client_view,
        cmd_tx: Mutex::new(cmd_tx),
    })
}

fn start_stream_cpal(
    ctx: &mut FirewheelContext,
    sample_rate: u32,
    output_block_frames: Option<NonZeroU32>,
) -> Result<CpalStream, String> {
    debug!(sample_rate, "[KITHARA-ROUTE] starting cpal stream");
    let config = cpal_config(sample_rate, output_block_frames);
    match CpalStream::new(ctx, config) {
        Ok(stream) => {
            debug!(sample_rate, "[KITHARA-ROUTE] cpal stream started");
            Ok(stream)
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

fn cpal_config(sample_rate: u32, output_block_frames: Option<NonZeroU32>) -> CpalConfig {
    let mut config = CpalConfig::default();
    config.output.desired_sample_rate = NonZeroU32::new(sample_rate).map(NonZeroU32::get);
    if let Some(frames) = output_block_frames {
        config.output.desired_block_frames = Some(frames.get());
    }
    config
}

pub(crate) fn spawn<S: HasPool<f32> + Send + Sync + 'static>(
    root: GroupState<PlayerMember>,
    root_view: RootView,
    sample_rate: NonZeroU32,
    output_block_frames: Option<NonZeroU32>,
    limiter: LimiterConfig,
) -> Arc<dyn HostDispatcher<S>> {
    spawn_session_client::<CpalStream, S>(
        "kithara-engine",
        root,
        root_view,
        sample_rate,
        output_block_frames,
        limiter,
        move |ctx, sample_rate| start_stream_cpal(ctx, sample_rate, output_block_frames),
    )
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn output_block_override_preserves_the_backend_default_or_sets_128() {
        let inherited = cpal_config(44_100, None);
        assert_eq!(
            inherited.output.desired_block_frames,
            firewheel::cpal::CpalOutputConfig::default().desired_block_frames
        );

        let frames = NonZeroU32::new(128).expect("test block size is non-zero");
        let configured = cpal_config(44_100, Some(frames));
        assert_eq!(configured.output.desired_block_frames, Some(128));
    }
}
