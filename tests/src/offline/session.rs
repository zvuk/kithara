use std::{num::NonZeroU32, ops::RangeInclusive};

use kithara::{
    audio::ConsumerWakeMode,
    bufpool::HasPool,
    host::{HostConfig, testing::GraphSession},
    platform::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        thread::{JoinHandle, spawn_named},
        time::{Duration, Instant},
    },
    play::{Cmd, MixTapWriter, PlayError, Reply, SessionDispatcher, StreamShape},
};
use ringbuf::{
    HeapCons, HeapRb,
    traits::{Consumer, Observer, Split},
};
use tracing::warn;

use super::backend::{OfflineBackend, OfflineConfig};
use crate::bufpool_ext::TestPools;

pub const OFFLINE_BLOCK_FRAMES: usize = 512;
pub const OFFLINE_PARK_MS: u64 = 10;

/// Liveness floor for a position-gain probe: well under one nominal
/// window, so it fails only when the auto-render effectively stalls.
const GAIN_FLOOR_SECS: f64 = 0.9;
/// Budget for everything a gain probe's endpoints cannot pin to the render
/// thread: both endpoints (progress events or position polls) are delivered
/// asynchronously, so the render keeps advancing across each endpoint's
/// delivery lag, and progress emits quantize position in ~100 ms steps.
const ENDPOINT_SLACK_SECS: f64 = 0.5;

/// Expected playback-position gain while auto-render advances at its
/// block-scaled reference cadence.
#[must_use]
pub fn offline_gain_window(window_secs: f64) -> RangeInclusive<f64> {
    let default_rate = GraphSession::<OfflineBackend, TestPools>::DEFAULT_SAMPLE_RATE;
    let block_frames = f64::from(u32::try_from(OFFLINE_BLOCK_FRAMES).unwrap_or(u32::MAX));
    let park_ms = f64::from(u32::try_from(OFFLINE_PARK_MS).unwrap_or(u32::MAX));
    let rate = (block_frames / f64::from(default_rate)) / (park_ms / 1_000.0);
    GAIN_FLOOR_SECS..=(rate * (window_secs + ENDPOINT_SLACK_SECS))
}

enum OfflineMsg<S> {
    Cmd {
        cmd: Cmd<S>,
        reply_tx: mpsc::Sender<Reply>,
    },
    Render {
        frames: usize,
        reply_tx: mpsc::Sender<Vec<f32>>,
    },
    Shutdown,
}

pub struct OfflineSession<S = TestPools> {
    cmd_tx: Mutex<mpsc::Sender<OfflineMsg<S>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl<S> OfflineSession<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// Auto-render mode: the worker periodically pulls one block of
    /// audio through the graph so playback advances even when the test
    /// thread never calls [`render`](Self::render).
    #[must_use]
    pub fn new() -> Self {
        Self::spawn(true, default_output_block_frames())
    }

    /// Manual mode: the worker only dispatches commands; the audio
    /// graph advances only when [`render`](Self::render) is called.
    #[must_use]
    pub fn new_manual() -> Self {
        Self::new_manual_with_block_frames(default_output_block_frames())
    }

    /// Manual mode with the audio callback size used by the scenario.
    #[must_use]
    pub fn new_manual_with_block_frames(block_frames: usize) -> Self {
        Self::spawn(false, block_frames)
    }

    /// Convenience: `Arc<dyn SessionDispatcher>` over a fresh
    /// auto-render session. Use when the test wires the dispatcher
    /// into [`kithara::play::EngineConfig::session`] and never calls
    /// [`render`](Self::render) directly.
    #[must_use]
    pub fn arc_auto() -> Arc<dyn SessionDispatcher<S>> {
        Arc::new(Self::new())
    }

    /// Convenience: `Arc<dyn SessionDispatcher>` over a fresh
    /// manual-render session. Use when the test owns rendering via
    /// [`render`](Self::render) (rare — most callers want an
    /// `Arc<OfflineSession>` so they can keep both the dyn handle and
    /// the render API).
    #[must_use]
    pub fn arc_manual() -> Arc<dyn SessionDispatcher<S>> {
        Arc::new(Self::new_manual())
    }

    fn spawn(auto_render: bool, block_frames: usize) -> Self {
        let block_frames = u32::try_from(block_frames).expect("offline block size fits u32");
        assert!(block_frames > 0, "offline block size must be non-zero");
        let (cmd_tx, cmd_rx) = mpsc::channel::<OfflineMsg<S>>();
        let handle = spawn_named("kithara-engine-offline-instance", move || {
            offline_session_thread(&cmd_rx, auto_render, block_frames);
        });
        Self {
            cmd_tx: Mutex::new(cmd_tx),
            worker: Mutex::new(Some(handle)),
        }
    }

    pub fn enable_mix_tap(&self, capacity: usize) -> Result<MixTapProbe, PlayError> {
        let (pcm_tx, pcm_rx) = HeapRb::<f32>::new(capacity).split();
        let drops = Arc::new(AtomicU64::new(0));
        self.exec_ok(Cmd::EnableMixTap {
            writer: MixTapWriter::new(pcm_tx, Arc::clone(&drops)),
        })?;
        Ok(MixTapProbe { drops, pcm: pcm_rx })
    }

    /// Synchronously render exactly `frames` frames, split into callbacks no
    /// larger than the session's declared output block. Returns stereo-interleaved
    /// samples, or an empty `Vec` if no player has started the firewheel context.
    /// `no_block`: sync command-reply bridge to the dedicated offline render thread; flash coordinates the bridged wait.
    #[kithara::allow_block]
    pub fn render(&self, frames: usize) -> Vec<f32> {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .cmd_tx
            .lock()
            .send(OfflineMsg::Render { frames, reply_tx })
            .is_err()
        {
            return Vec::new();
        }
        reply_rx.recv().unwrap_or_default()
    }
}

impl<S> Default for OfflineSession<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

pub struct MixTapProbe {
    drops: Arc<AtomicU64>,
    pcm: HeapCons<f32>,
}

impl MixTapProbe {
    pub fn drain(&mut self) -> Vec<f32> {
        self.pcm.pop_iter().collect()
    }

    #[must_use]
    pub fn drops(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn writer_alive(&self) -> bool {
        self.pcm.write_is_held()
    }
}

impl<S> Drop for OfflineSession<S> {
    fn drop(&mut self) {
        // `Shutdown` lands on the engine-aware command channel; `run_auto`/
        // `run_manual` block on its arrival event (`recv`/`recv_timeout`),
        // so the render thread wakes the instant this send signals the condvar —
        // no `unpark`, and no dependence on a virtual-clock advance the joining
        // thread below would otherwise pin.
        let _ = self.cmd_tx.lock().send(OfflineMsg::Shutdown);
        if let Some(handle) = self.worker.lock().take() {
            let _ = handle.join();
        }
    }
}

impl<S> SessionDispatcher<S> for OfflineSession<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// The render thread runs the device callback's processor.
    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }

    /// `no_block`: sync command-reply bridge to the dedicated offline render thread; flash coordinates the bridged wait.
    #[kithara::allow_block]
    fn exec(&self, cmd: Cmd<S>) -> Result<Reply, PlayError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .lock()
            .send(OfflineMsg::Cmd { cmd, reply_tx })
            .map_err(|_| PlayError::Internal("offline session worker gone".into()))?;
        reply_rx
            .recv()
            .map_err(|_| PlayError::Internal("offline session worker gone (reply)".into()))
    }
}

fn start_stream_offline(
    ctx: &mut firewheel::FirewheelCtx<OfflineBackend>,
    sample_rate: u32,
    block_frames: u32,
) -> Result<(), String> {
    let config = OfflineConfig {
        block_frames,
        sample_rate,
    };
    ctx.start_stream(config).map_err(|err| err.to_string())
}

fn offline_session_thread<S>(
    cmd_rx: &mpsc::Receiver<OfflineMsg<S>>,
    auto_render: bool,
    block_frames: u32,
) where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let requested_shape = StreamShape::new(
        NonZeroU32::new(block_frames).expect("offline block size was validated as non-zero"),
        NonZeroU32::new(GraphSession::<OfflineBackend, S>::DEFAULT_SAMPLE_RATE)
            .expect("offline sample rate is non-zero"),
    );
    let mut state = GraphSession::<OfflineBackend, S>::with_stream_shape(
        requested_shape,
        move |ctx, sample_rate| start_stream_offline(ctx, sample_rate, block_frames),
    );
    if auto_render {
        run_auto(
            &mut state,
            cmd_rx,
            usize::try_from(block_frames).expect("offline block size fits usize"),
        );
    } else {
        run_manual(
            &mut state,
            cmd_rx,
            usize::try_from(block_frames).expect("offline block size fits usize"),
        );
    }
}

fn run_manual<S>(
    state: &mut GraphSession<OfflineBackend, S>,
    cmd_rx: &mpsc::Receiver<OfflineMsg<S>>,
    block_frames: usize,
) where
    S: HasPool<f32> + Send + Sync + 'static,
{
    for msg in cmd_rx.iter() {
        match msg {
            OfflineMsg::Cmd { cmd, reply_tx } => {
                let reply = state.exec(cmd);
                let _ = reply_tx.send(reply);
            }
            OfflineMsg::Render { frames, reply_tx } => {
                let block = render_frames(state, frames, block_frames);
                let _ = reply_tx.send(block);
            }
            OfflineMsg::Shutdown => break,
        }
    }
}

fn run_auto<S>(
    state: &mut GraphSession<OfflineBackend, S>,
    cmd_rx: &mpsc::Receiver<OfflineMsg<S>>,
    block_frames: usize,
) where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let render_period = auto_render_period(block_frames);
    loop {
        // Block on the next command, but no longer than one render budget: a
        // command (or `Shutdown`) wakes us at once through the engine-aware
        // channel, while the timeout drives the periodic auto-render so playback
        // advances even when the test thread never sends anything. There is no
        // `park_timeout` to lose a cross-thread wake against.
        let deadline = Instant::now() + render_period;
        match cmd_rx.recv_timeout(deadline) {
            Ok(OfflineMsg::Cmd { cmd, reply_tx }) => {
                let reply = state.exec(cmd);
                let _ = reply_tx.send(reply);
            }
            Ok(OfflineMsg::Render { frames, reply_tx }) => {
                let block = render_frames(state, frames, block_frames);
                let _ = reply_tx.send(block);
            }
            Ok(OfflineMsg::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = render_block(state, block_frames);
            }
        }
    }
}

fn auto_render_period(block_frames: usize) -> Duration {
    let block_frames = f64::from(
        u32::try_from(block_frames).expect("offline callback block size originated as u32"),
    );
    let reference_frames = f64::from(
        u32::try_from(OFFLINE_BLOCK_FRAMES).expect("offline reference block size fits u32"),
    );
    Duration::from_millis(OFFLINE_PARK_MS).mul_f64(block_frames / reference_frames)
}

fn default_output_block_frames() -> usize {
    usize::try_from(HostConfig::builder().build().output_block_frames().get())
        .expect("offline block size fits usize")
}

fn render_frames<S>(
    state: &mut GraphSession<OfflineBackend, S>,
    frames: usize,
    block_frames: usize,
) -> Vec<f32>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let mut output = Vec::new();
    let mut remaining = frames;
    while remaining > 0 {
        let frames = remaining.min(block_frames);
        output.extend(render_block(state, frames));
        remaining -= frames;
    }
    output
}

fn render_block<S>(state: &mut GraphSession<OfflineBackend, S>, frames: usize) -> Vec<f32>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    let Some(ctx) = state.ctx_mut() else {
        return Vec::new();
    };
    if let Err(err) = ctx.update() {
        warn!("offline session graph update failed: {err:?}");
    }
    ctx.active_backend_mut()
        .map_or_else(Vec::new, |backend| backend.render(frames))
}

#[cfg(test)]
mod tests {
    use kithara::audio::ConsumerWakeMode;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn offline_session_requests_realtime_deferred_consumer_wakes() {
        let session = OfflineSession::<TestPools>::new_manual();

        assert_eq!(
            session.consumer_wake_mode(),
            ConsumerWakeMode::RealtimeDeferred
        );
    }

    #[kithara::test(native, flash(false))]
    fn default_offline_session_uses_host_callback_geometry() {
        let session = OfflineSession::<TestPools>::new();
        let Reply::StreamShape(shape) = session
            .exec(Cmd::QueryStreamShape)
            .expect("offline session answers the stream-shape query")
        else {
            panic!("offline session answers with its stream shape");
        };

        assert_eq!(
            shape.max_block_frames,
            HostConfig::builder().build().output_block_frames()
        );
    }

    #[kithara::test(native, flash(false))]
    fn gain_window_floor_rejects_a_stalled_render() {
        assert!(!offline_gain_window(2.0).contains(&0.8));
    }

    #[kithara::test(native, flash(false))]
    fn gain_window_admits_the_nominal_cadence() {
        assert!(offline_gain_window(2.0).contains(&2.32));
    }

    /// Both probe endpoints are delivered asynchronously to the render
    /// thread, so under host-scheduler perturbation the render advances
    /// past the pure-window nominal before an endpoint lands. Stress runs
    /// 32703368671 / 32718477852 / 32739592529 measured 2.501–2.509 s over
    /// a 2 s window this way — a legitimate outcome, not a cadence defect.
    #[kithara::test(native, flash(false))]
    fn gain_window_admits_endpoint_delivery_lag() {
        assert!(offline_gain_window(2.0).contains(&2.51));
    }

    #[kithara::test(native, flash(false))]
    fn gain_window_rejects_a_double_cadence() {
        assert!(!offline_gain_window(2.0).contains(&4.64));
    }
}
