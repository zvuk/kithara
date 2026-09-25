use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};

use bon::Builder;
use kithara_bufpool::PoolRegion;
use kithara_encode::EncodeConfig;
use kithara_platform::time::Duration;
use kithara_worker::{Priority, Worker};

use crate::PartSinkFactory;

struct Defaults;

impl Defaults {
    const BUFFER_FRAMES: NonZeroUsize = match NonZeroUsize::new(96_000) {
        Some(value) => value,
        None => unreachable!(),
    };
    const DISPATCHER_CAPACITY: NonZeroUsize = NonZeroUsize::MIN;
    const FAIRNESS_YIELD_INTERVAL: NonZeroU32 = match NonZeroU32::new(16) {
        Some(value) => value,
        None => unreachable!(),
    };
    const GENERATION_CAPACITY: NonZeroUsize = match NonZeroUsize::new(8) {
        Some(value) => value,
        None => unreachable!(),
    };
    const TICK_FRAMES: NonZeroUsize = match NonZeroUsize::new(1_024) {
        Some(value) => value,
        None => unreachable!(),
    };
}

/// Configuration for one independently playable recording part.
#[kithara_config::config(builder = false)]
#[derive(Clone, Debug, Builder)]
#[non_exhaustive]
pub struct RecordingConfig {
    #[builder(default = default_encode_config())]
    #[config(nested)]
    encode: EncodeConfig,
}

impl RecordingConfig {
    /// Encoding profile for this part.
    #[must_use]
    pub const fn encode(&self) -> &EncodeConfig {
        &self.encode
    }

    pub(crate) fn set_sample_rate(&mut self, sample_rate: u32) {
        self.encode.sample_rate = sample_rate;
    }
}

fn default_encode_config() -> EncodeConfig {
    EncodeConfig::builder()
        .sample_rate(48_000)
        .channels(2)
        .build()
}

/// Bounded live-recorder and worker scheduling configuration.
#[kithara_config::config(construction, builder = false)]
#[derive(Builder)]
#[non_exhaustive]
pub struct LiveRecordingConfig<F, S>
where
    F: PartSinkFactory,
{
    /// Shared worker used to schedule the recorder task.
    #[builder(start_fn)]
    #[config(skip = "injected worker")]
    pub(crate) worker: Worker,
    /// Typed pool facade used for bounded recorder scratch.
    #[builder(start_fn)]
    #[config(skip = "injected buffer pools")]
    pub(crate) pools: PoolRegion<S>,
    /// Factory opening each transactional recording part.
    #[builder(start_fn)]
    #[config(skip = "injected part sink factory")]
    pub(crate) factory: F,
    /// Dispatcher park duration when the recorder has no work.
    #[builder(default = Duration::from_millis(100))]
    #[config(value)]
    pub(crate) idle_timeout: Duration,
    /// Threshold for reporting a slow recorder tick.
    #[builder(default = Duration::from_millis(10))]
    #[config(value)]
    pub(crate) slow_tick_threshold: Duration,
    /// Dispatcher wait duration between deferred RT wakes.
    #[builder(default = Duration::from_millis(10))]
    #[config(value)]
    pub(crate) wait_timeout: Duration,
    /// Consecutive progress passes before the dispatcher yields.
    #[builder(default = Defaults::FAIRNESS_YIELD_INTERVAL)]
    #[config(value)]
    pub(crate) fairness_yield_interval: NonZeroU32,
    /// Maximum consecutive recorder ticks in one dispatcher visit.
    #[builder(default = NonZeroU32::MIN)]
    #[config(value)]
    pub(crate) task_burst: NonZeroU32,
    /// Maximum stereo PCM frames waiting between RT and the encoder worker.
    #[builder(default = Defaults::BUFFER_FRAMES)]
    #[config(value)]
    pub(crate) buffer_frames: NonZeroUsize,
    /// Maximum tasks admitted to the recorder dispatcher.
    #[builder(default = Defaults::DISPATCHER_CAPACITY)]
    #[config(value)]
    pub(crate) dispatcher_capacity: NonZeroUsize,
    /// Maximum queued master-format generations waiting for the worker.
    #[builder(default = Defaults::GENERATION_CAPACITY)]
    #[config(value)]
    pub(crate) generation_capacity: NonZeroUsize,
    /// Maximum compute jobs admitted for the recorder task.
    #[builder(default = NonZeroUsize::MIN)]
    #[config(value)]
    pub(crate) max_compute_tasks: NonZeroUsize,
    /// Maximum stereo PCM frames encoded during one worker tick.
    #[builder(default = Defaults::TICK_FRAMES)]
    #[config(value)]
    pub(crate) tick_frames: NonZeroUsize,
    /// Optional exact frame count at which each part rotates automatically.
    #[config(value)]
    pub(crate) rotation_frames: Option<NonZeroU64>,
    /// Recorder task priority.
    #[builder(default = Priority::new(0))]
    #[config(value)]
    pub(crate) priority: Priority,
    /// Encoding and container profile for every independently playable part.
    #[builder(default = RecordingConfig::builder().build())]
    #[config(nested)]
    pub(crate) recording: RecordingConfig,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::RecordingConfig;

    #[kithara::test(native, flash(false))]
    fn recording_recipe_snapshot_tracks_measured_sample_rate() {
        let mut config = RecordingConfig::builder().build();
        let initial = kithara_config::Config::values(&config);
        assert_eq!(initial.encode.sample_rate, 48_000);
        assert_eq!(initial.encode.channels, 2);

        config.set_sample_rate(44_100);
        let measured = kithara_config::Config::values(&config);
        assert_eq!(measured.encode.sample_rate, 44_100);
        assert_eq!(measured.encode.codec, initial.encode.codec);
        assert_eq!(measured.encode.container, initial.encode.container);
    }
}
