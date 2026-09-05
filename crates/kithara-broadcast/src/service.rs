use std::num::NonZeroU32;

use kithara_bufpool::HasPool;
use kithara_output::LiveOutput;
use kithara_platform::{
    CancelGroup, CancelScope, CancelToken,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    time::Instant,
};
use kithara_signal::AudioSpec;
use kithara_test_utils::kithara;
use kithara_worker::{Dispatcher, DispatcherConfig, TaskConfig, TaskHandle, Wake};
use ringbuf::{
    HeapProd, HeapRb,
    traits::{Observer, Producer, Split},
};

use self::task::BroadcastTask;
use crate::{
    BroadcastError, BroadcastResult,
    config::BroadcastConfig,
    server::{self, Origin},
};

mod task;

struct Consts;

impl Consts {
    const CHANNELS: u16 = 2;
    const STEREO: usize = 2;
}

#[derive(Clone, Copy)]
pub(super) struct FormatChange {
    pub(super) frame: u64,
    pub(super) spec: AudioSpec,
}

/// Current public state of a broadcast.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BroadcastStatus {
    /// Master playlist URL.
    pub url: Arc<str>,
    /// The stream is still taking audio; `false` once the tail is a VOD
    /// playlist.
    pub is_live: bool,
    /// Interleaved PCM samples dropped at the bounded RT intake.
    pub dropped_samples: u64,
    /// Closed media segments published by the packager.
    pub segments: u64,
}

#[derive(Default)]
pub(super) struct Control {
    accepting: AtomicBool,
    dropped: AtomicU64,
    finish_requested: AtomicBool,
    generation_overflowed: AtomicBool,
    writing: AtomicBool,
}

impl Control {
    fn finish(&self) {
        self.accepting.store(false, Ordering::Release);
        self.finish_requested.store(true, Ordering::Release);
    }

    pub(super) fn is_finished(&self, pcm_is_empty: bool) -> bool {
        self.finish_requested.load(Ordering::Acquire)
            && !self.writing.load(Ordering::Acquire)
            && pcm_is_empty
    }

    pub(super) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Acquire)
    }
}

#[derive(Debug, Default)]
pub(super) struct Counters {
    pub(super) dropped: AtomicU64,
    pub(super) segments: AtomicU64,
}

/// RT endpoint installed in the Host master-output group.
pub struct BroadcastOutput {
    control: Arc<Control>,
    formats: HeapProd<FormatChange>,
    pcm: HeapProd<f32>,
    spec: AudioSpec,
    written_frames: u64,
    wake: Wake,
}

impl BroadcastOutput {
    fn report_drop(&self, dropped: usize) {
        let dropped = u64::try_from(dropped).unwrap_or(u64::MAX);
        let mut total = self.control.dropped.load(Ordering::Relaxed);
        while let Err(current) = self.control.dropped.compare_exchange_weak(
            total,
            total.saturating_add(dropped),
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            total = current;
        }
    }
}

impl LiveOutput for BroadcastOutput {
    fn reconfigure(&mut self, spec: AudioSpec) {
        if spec == self.spec || !self.control.accepting.load(Ordering::Acquire) {
            return;
        }
        let change = FormatChange {
            frame: self.written_frames,
            spec,
        };
        if self.formats.try_push(change).is_err() {
            self.control
                .generation_overflowed
                .store(true, Ordering::Release);
            self.control.finish();
        } else {
            self.spec = spec;
        }
        self.wake.defer();
    }

    fn write_stereo(&mut self, frames: usize, left: &[f32], right: &[f32]) {
        if frames == 0 || !self.control.accepting.load(Ordering::Acquire) {
            return;
        }
        self.control.writing.store(true, Ordering::Release);
        if !self.control.accepting.load(Ordering::Acquire) {
            self.control.writing.store(false, Ordering::Release);
            return;
        }

        let writable = frames
            .min(left.len())
            .min(right.len())
            .min(self.pcm.vacant_len() / Consts::STEREO);
        let pushed = self.pcm.push_iter(
            left[..writable]
                .iter()
                .zip(&right[..writable])
                .flat_map(|(&left, &right)| [left, right]),
        );
        let requested = frames.saturating_mul(Consts::STEREO);
        let dropped = requested.saturating_sub(pushed);
        if dropped > 0 {
            self.report_drop(dropped);
        }
        let pushed_frames = pushed / Consts::STEREO;
        self.written_frames = self
            .written_frames
            .saturating_add(u64::try_from(pushed_frames).unwrap_or(u64::MAX));
        self.control.writing.store(false, Ordering::Release);
        self.wake.defer();
    }
}

impl Drop for BroadcastOutput {
    fn drop(&mut self) {
        self.control.finish();
        self.wake.defer();
    }
}

/// Live HLS origin entry point.
pub struct Broadcast;

impl Broadcast {
    /// Start one configured bounded packager and bind the origin before
    /// returning its output and handle.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration, encoding, binding, or worker
    /// admission fails.
    pub fn start<S>(
        config: BroadcastConfig<S>,
    ) -> BroadcastResult<(BroadcastOutput, BroadcastHandle)>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        config.validate()?;
        if config.channels != Consts::CHANNELS {
            return Err(BroadcastError::LiveChannelCount {
                channels: config.channels,
            });
        }
        let buffer_samples = config
            .buffer_frames
            .get()
            .checked_mul(Consts::STEREO)
            .ok_or(BroadcastError::CapacityOverflow)?;
        let tick_samples = config
            .tick_frames
            .get()
            .checked_mul(Consts::STEREO)
            .ok_or(BroadcastError::CapacityOverflow)?;
        let scratch = config.pools.get_with_len::<f32>(tick_samples)?;
        let (pcm_tx, pcm_rx) = HeapRb::new(buffer_samples).split();
        let (format_tx, format_rx) = HeapRb::new(config.generation_capacity.get()).split();
        let control = Arc::new(Control {
            accepting: AtomicBool::new(true),
            ..Control::default()
        });
        let (completed_tx, completed_rx) = mpsc::channel();
        let scope = CancelScope::new(config.cancel.clone());
        let bind = config.bind;
        let channels = config.channels;
        let sample_rate = config.sample_rate;
        let stop_timeout = config.stop_timeout;
        let dispatcher = config.worker.dispatcher(
            DispatcherConfig::builder()
                .name("kithara-broadcast")
                .cancel(CancelGroup::from(scope.token()))
                .capacity(config.dispatcher_capacity)
                .fairness_yield_interval(config.fairness_yield_interval)
                .idle_timeout(config.idle_timeout)
                .slow_tick_threshold(config.slow_tick_threshold)
                .task_burst(config.task_burst)
                .wait_timeout(config.wait_timeout)
                .build(),
        );
        let wake = dispatcher.wake_handle();
        let task_config = TaskConfig::new()
            .with_max_compute_tasks(config.max_compute_tasks)
            .with_priority(config.priority);
        let task = BroadcastTask::new(
            config,
            pcm_rx,
            format_rx,
            Arc::clone(&control),
            scratch,
            completed_tx,
        )?;
        let origin = task.origin();
        let counters = task.counters();
        let addr = match server::start(bind, Arc::clone(&origin), scope.token()) {
            Ok(addr) => addr,
            Err(error) => {
                scope.cancel();
                return Err(error);
            }
        };
        let task = match dispatcher.register(task_config, move |_| task) {
            Ok(task) => task,
            Err(error) => {
                scope.cancel();
                return Err(error.into());
            }
        };
        let output = BroadcastOutput {
            control: Arc::clone(&control),
            formats: format_tx,
            pcm: pcm_tx,
            spec: AudioSpec::new(
                channels,
                NonZeroU32::new(sample_rate).ok_or(BroadcastError::InvalidConfig {
                    field: "sample_rate",
                })?,
            ),
            written_frames: 0,
            wake: wake.clone(),
        };
        let handle = BroadcastHandle {
            completed: Mutex::new(Some(completed_rx)),
            control,
            counters,
            dispatcher,
            origin,
            scope,
            stop_timeout,
            task,
            url: Arc::from(format!("http://{addr}/master.m3u8")),
            wake,
        };
        Ok((output, handle))
    }
}

/// Handle owning a live broadcast and its worker/origin lifecycle.
pub struct BroadcastHandle {
    completed: Mutex<Option<mpsc::Receiver<()>>>,
    control: Arc<Control>,
    counters: Arc<Counters>,
    dispatcher: Dispatcher,
    origin: Arc<Origin>,
    scope: CancelScope,
    stop_timeout: kithara_platform::time::Duration,
    task: TaskHandle,
    url: Arc<str>,
    wake: Wake,
}

impl BroadcastHandle {
    /// What the origin is serving right now.
    #[must_use]
    pub fn status(&self) -> BroadcastStatus {
        BroadcastStatus {
            url: Arc::clone(&self.url),
            is_live: !self.origin.snapshot.load().is_finished,
            segments: self.counters.segments.load(Ordering::Relaxed),
            dropped_samples: self.counters.dropped.load(Ordering::Relaxed),
        }
    }

    /// Finish handed-over audio and keep serving the resulting VOD tail.
    /// Repeated calls have no effect.
    /// `no_block`: bounded synchronous bridge used from blocking app tasks.
    #[kithara::allow_block]
    pub fn stop(&self) {
        self.control.finish();
        self.wake.wake();
        let completed = self.completed.lock().take();
        let Some(completed) = completed else {
            return;
        };
        let deadline = Instant::now() + self.stop_timeout;
        match completed.recv_timeout(deadline) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {
                tracing::error!(timeout = ?self.stop_timeout, "the broadcast tail did not drain");
                self.task.cancel();
                self.scope.cancel();
            }
            Err(RecvTimeoutError::Disconnected) => {
                tracing::error!("the broadcast worker stopped without a completion report");
                self.scope.cancel();
            }
        }
    }

    /// Cancel token the broadcast's worker and origin run under.
    #[must_use]
    pub fn token(&self) -> CancelToken {
        self.scope.token()
    }

    /// Master playlist URL a player joins the stream at.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for BroadcastHandle {
    fn drop(&mut self) {
        self.control.finish();
        self.scope.cancel();
        self.task.cancel();
        self.dispatcher.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::testing::{TestPools, pools};
    use kithara_output::LiveOutput;
    use kithara_platform::time::Duration;
    use kithara_stream::{AudioCodec, ContainerFormat};
    use kithara_test_utils::kithara;
    use kithara_worker::{Worker, WorkerConfig};

    use super::*;

    struct Consts;

    impl Consts {
        const AMPLITUDE: f32 = 0.25;
        const SAMPLE_RATE: usize = 48_000;
        const TARGET: Duration = Duration::from_millis(500);
    }

    fn config() -> BroadcastConfig<TestPools> {
        BroadcastConfig::builder(Worker::new(WorkerConfig::new()), pools())
            .segment_target(Consts::TARGET)
            .build()
    }

    fn playlist(handle: &BroadcastHandle) -> Arc<str> {
        Arc::clone(&handle.origin.snapshot.load().playlist)
    }

    fn start() -> (BroadcastOutput, BroadcastHandle) {
        Broadcast::start(config()).expect("on air")
    }

    #[kithara::test(native, flash(false))]
    fn an_unsupported_profile_fails_without_fallback() {
        let config = BroadcastConfig::builder(Worker::new(WorkerConfig::new()), pools())
            .codec(AudioCodec::Pcm)
            .container(ContainerFormat::Wav)
            .build();

        assert!(matches!(
            Broadcast::start(config),
            Err(BroadcastError::UnsupportedProfile {
                codec: AudioCodec::Pcm,
                container: ContainerFormat::Wav,
            })
        ));
    }

    fn write_second(output: &mut impl LiveOutput) {
        let left = vec![Consts::AMPLITUDE; Consts::SAMPLE_RATE];
        let right = vec![-Consts::AMPLITUDE; Consts::SAMPLE_RATE];
        output.write_stereo(left.len(), &left, &right);
    }

    #[kithara::test(native, flash(false))]
    fn live_output_is_the_public_pcm_intake() {
        let (mut output, handle) = start();

        write_second(&mut output);
        handle.stop();

        assert!(handle.status().segments > 0);
        assert!(playlist(&handle).contains("#EXT-X-ENDLIST\n"));
    }

    #[kithara::test(native, flash(false))]
    fn dropping_the_output_finishes_the_stream() {
        let (mut output, handle) = start();
        write_second(&mut output);

        drop(output);
        handle.stop();

        assert!(!handle.status().is_live);
        assert!(playlist(&handle).contains("#EXT-X-ENDLIST\n"));
    }

    #[kithara::test(native, flash(false))]
    fn an_intake_gap_marks_the_next_segment_discontinuous() {
        let (mut output, handle) = start();
        write_second(&mut output);
        output.write_stereo(Consts::SAMPLE_RATE, &[], &[]);
        write_second(&mut output);

        handle.stop();

        assert!(playlist(&handle).contains("#EXT-X-DISCONTINUITY\n"));
        assert_eq!(
            handle.status().dropped_samples,
            u64::try_from(Consts::SAMPLE_RATE * 2).expect("drop count fits")
        );
    }

    #[kithara::test(native, flash(false))]
    fn stopping_twice_is_the_same_as_stopping_once() {
        let (mut output, handle) = start();
        write_second(&mut output);

        handle.stop();
        let after_first = handle.status();
        handle.stop();

        assert!(!after_first.is_live);
        assert_eq!(handle.status().segments, after_first.segments);
    }
}
