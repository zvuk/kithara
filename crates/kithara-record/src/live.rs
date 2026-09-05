use std::num::NonZeroU32;

use kithara_bufpool::HasPool;
use kithara_output::LiveOutput;
use kithara_platform::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use kithara_signal::AudioSpec;
use kithara_worker::{Dispatcher, DispatcherConfig, TaskConfig, TaskHandle, Wake, Worker};
use ringbuf::{
    HeapProd, HeapRb,
    traits::{Observer, Producer, Split},
};

use self::task::RecordingTask;
use crate::{LiveRecordingConfig, LiveRecordingError, PartSinkFactory};

mod task;

struct Consts;

impl Consts {
    const CHANNELS: u16 = 2;
    const NO_CUT: u64 = u64::MAX;
    const STEREO: usize = 2;
}

#[derive(Clone, Copy)]
pub(super) struct FormatChange {
    pub(super) frame: u64,
    pub(super) spec: AudioSpec,
}

/// Completed live-recording counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct LiveRecordingReport {
    /// Total complete stereo frames committed across all parts.
    pub frames: u64,
    /// Number of independently playable parts committed.
    pub parts: u64,
}

type ResultSlot = Arc<Mutex<Option<Result<LiveRecordingReport, LiveRecordingError>>>>;

#[derive(Default)]
struct Control {
    accepting: AtomicBool,
    cut_at: AtomicU64,
    cut_requested: AtomicBool,
    finish_requested: AtomicBool,
    generation_overflowed: AtomicBool,
    overflowed: AtomicBool,
    result: ResultSlot,
    writing: AtomicBool,
    written_frames: AtomicU64,
}

impl Control {
    fn finish(&self) {
        self.accepting.store(false, Ordering::Release);
        self.finish_requested.store(true, Ordering::Release);
    }
}

/// RT endpoint copied into the Host master-output group.
pub struct RecordingOutput {
    control: Arc<Control>,
    formats: HeapProd<FormatChange>,
    pcm: HeapProd<f32>,
    spec: AudioSpec,
    wake: Wake,
}

impl RecordingOutput {
    fn overflow(&self) {
        self.control.overflowed.store(true, Ordering::Release);
        self.control.accepting.store(false, Ordering::Release);
        self.wake.defer();
    }
}

impl LiveOutput for RecordingOutput {
    fn reconfigure(&mut self, spec: AudioSpec) {
        if spec == self.spec || !self.control.accepting.load(Ordering::Acquire) {
            return;
        }
        let change = FormatChange {
            frame: self.control.written_frames.load(Ordering::Acquire),
            spec,
        };
        if self.formats.try_push(change).is_err() {
            self.control
                .generation_overflowed
                .store(true, Ordering::Release);
            self.control.accepting.store(false, Ordering::Release);
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
        let Some(samples) = frames.checked_mul(Consts::STEREO) else {
            self.control.writing.store(false, Ordering::Release);
            self.overflow();
            return;
        };
        if left.len() < frames || right.len() < frames || self.pcm.vacant_len() < samples {
            self.control.writing.store(false, Ordering::Release);
            self.overflow();
            return;
        }

        if self.control.cut_requested.swap(false, Ordering::AcqRel) {
            let at = self.control.written_frames.load(Ordering::Acquire);
            if self
                .control
                .cut_at
                .compare_exchange(Consts::NO_CUT, at, Ordering::Release, Ordering::Relaxed)
                .is_err()
            {
                self.control.cut_requested.store(true, Ordering::Release);
            }
        }

        let pushed = self.pcm.push_iter(
            left[..frames]
                .iter()
                .zip(&right[..frames])
                .flat_map(|(&left, &right)| [left, right]),
        );
        if pushed != samples {
            self.control.writing.store(false, Ordering::Release);
            self.overflow();
            return;
        }
        let Ok(frames) = u64::try_from(frames) else {
            self.control.writing.store(false, Ordering::Release);
            self.overflow();
            return;
        };
        let mut written = self.control.written_frames.load(Ordering::Relaxed);
        loop {
            let Some(next) = written.checked_add(frames) else {
                self.control.writing.store(false, Ordering::Release);
                self.overflow();
                return;
            };
            match self.control.written_frames.compare_exchange_weak(
                written,
                next,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => written = current,
            }
        }
        self.control.writing.store(false, Ordering::Release);
        self.wake.defer();
    }
}

impl Drop for RecordingOutput {
    fn drop(&mut self) {
        self.control.finish();
        self.wake.defer();
    }
}

/// Off-RT recording controls and completion observation.
pub struct LiveRecordingHandle {
    control: Arc<Control>,
    dispatcher: Dispatcher,
    task: TaskHandle,
    wake: Wake,
    _worker: Worker,
}

impl LiveRecordingHandle {
    /// Request a part boundary before the next observable audio block.
    pub fn cut(&self) {
        if self.control.accepting.load(Ordering::Acquire) {
            self.control.cut_requested.store(true, Ordering::Release);
        }
    }

    /// Stop accepting PCM and take the terminal result when it is ready.
    #[must_use]
    pub fn finish(&self) -> Option<Result<LiveRecordingReport, LiveRecordingError>> {
        self.control.finish();
        self.wake.wake();
        self.control.result.lock().take()
    }
}

impl Drop for LiveRecordingHandle {
    fn drop(&mut self) {
        self.task.cancel();
        self.dispatcher.shutdown();
    }
}

/// Starts one bounded live-recorder worker and its RT endpoint.
pub struct LiveRecorder;

impl LiveRecorder {
    /// Start recording with the configured worker, pools, and part factory.
    ///
    /// # Errors
    /// Returns an invalid configuration or worker admission failure.
    pub fn start<F, S>(
        config: LiveRecordingConfig<F, S>,
    ) -> Result<(RecordingOutput, LiveRecordingHandle), LiveRecordingError>
    where
        F: PartSinkFactory,
        S: HasPool<f32> + Send + Sync + 'static,
    {
        if config.recording.encode().channels != Consts::CHANNELS {
            return Err(LiveRecordingError::ChannelCount(
                config.recording.encode().channels,
            ));
        }
        let buffer_frames = config.buffer_frames.get();
        let buffer_samples = buffer_frames
            .checked_mul(Consts::STEREO)
            .ok_or(LiveRecordingError::CapacityOverflow)?;
        let tick_samples = config
            .tick_frames
            .get()
            .checked_mul(Consts::STEREO)
            .ok_or(LiveRecordingError::CapacityOverflow)?;
        let scratch = config.pools.get_with_len::<f32>(tick_samples)?;
        let (pcm_tx, pcm_rx) = HeapRb::new(buffer_samples).split();
        let (format_tx, format_rx) = HeapRb::new(config.generation_capacity.get()).split();
        let sample_rate = NonZeroU32::new(config.recording.encode().sample_rate)
            .ok_or(LiveRecordingError::InvalidSampleRate)?;
        let spec = AudioSpec::new(config.recording.encode().channels, sample_rate);
        let worker = config.worker.clone();
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name("kithara-record")
                .capacity(config.dispatcher_capacity)
                .fairness_yield_interval(config.fairness_yield_interval)
                .idle_timeout(config.idle_timeout)
                .slow_tick_threshold(config.slow_tick_threshold)
                .task_burst(config.task_burst)
                .wait_timeout(config.wait_timeout)
                .build(),
        );
        let wake = dispatcher.wake_handle();
        let control = Arc::new(Control {
            accepting: AtomicBool::new(true),
            cut_at: AtomicU64::new(Consts::NO_CUT),
            ..Control::default()
        });
        let task_control = Arc::clone(&control);
        let task = dispatcher.register(
            TaskConfig::new()
                .with_max_compute_tasks(config.max_compute_tasks)
                .with_priority(config.priority),
            move |_| {
                RecordingTask::new(
                    config,
                    pcm_rx,
                    format_rx,
                    task_control,
                    buffer_frames,
                    scratch,
                )
            },
        )?;
        let output = RecordingOutput {
            control: Arc::clone(&control),
            formats: format_tx,
            pcm: pcm_tx,
            spec,
            wake: wake.clone(),
        };
        let handle = LiveRecordingHandle {
            control,
            dispatcher,
            task,
            wake,
            _worker: worker,
        };
        Ok((output, handle))
    }
}
