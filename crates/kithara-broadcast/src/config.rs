use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::{NonZeroU32, NonZeroUsize},
};

use bon::Builder;
use kithara_bufpool::PoolRegion;
use kithara_macros::Patch;
use kithara_platform::{CancelToken, time::Duration};
use kithara_stream::{AudioCodec, ContainerFormat};
use kithara_worker::{Priority, Worker};

use crate::{BroadcastError, BroadcastResult};

/// Audio, segmentation, retention, and origin settings for a live broadcast.
///
/// [`BroadcastConfigPatch`] is what a configuration document may say about it.
#[derive(Builder, Patch)]
#[non_exhaustive]
pub struct BroadcastConfig<S> {
    /// Shared worker used to schedule the packager task.
    #[builder(start_fn)]
    #[patch(skip)]
    pub worker: Worker,
    /// Typed pool facade used for bounded packager scratch.
    #[builder(start_fn)]
    #[patch(skip)]
    pub pools: PoolRegion<S>,
    /// Optional cancellation parent for the broadcast lifetime.
    #[patch(skip)]
    pub cancel: Option<CancelToken>,
    /// Media duration a segment is cut at.
    #[builder(default = Duration::from_secs(4))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub segment_target: Duration,
    /// Loopback on an ephemeral port.
    #[builder(default = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))]
    pub bind: SocketAddr,
    /// Channel count of the mix.
    #[builder(default = 2)]
    pub channels: u16,
    /// Sample rate of the mix. Not a document key: the packager overwrites it
    /// with the master format it measured, so a named value would not survive
    /// the first format change.
    #[builder(default = 48_000)]
    #[patch(skip)]
    pub sample_rate: u32,
    /// AAC-LC bit rate the encoder targets.
    #[builder(default = 128_000)]
    pub bit_rate: u64,
    /// Codec emitted into HLS media segments. Not a document key:
    /// [`BroadcastConfig::validate`] admits one profile, so every value a
    /// document could name but the default is refused at startup.
    #[builder(default = AudioCodec::AacLc)]
    #[patch(skip)]
    pub codec: AudioCodec,
    /// Container carried by HLS media segments. Not a document key for the
    /// same reason [`Self::codec`] is not.
    #[builder(default = ContainerFormat::Adts)]
    #[patch(skip)]
    pub container: ContainerFormat,
    /// Segments kept fetchable past the playlist window.
    #[builder(default = 3)]
    pub grace: usize,
    /// Segments a client sees in the playlist.
    #[builder(default = 6)]
    pub window: usize,
    /// Maximum stereo PCM frames waiting between RT and the packager worker.
    #[builder(default = Defaults::BUFFER_FRAMES)]
    pub buffer_frames: NonZeroUsize,
    /// Maximum stereo PCM frames packaged during one worker tick.
    #[builder(default = Defaults::TICK_FRAMES)]
    pub tick_frames: NonZeroUsize,
    /// Maximum queued master-format generations waiting for the packager.
    #[builder(default = Defaults::GENERATION_CAPACITY)]
    pub generation_capacity: NonZeroUsize,
    /// Maximum tasks admitted to the broadcast dispatcher.
    #[builder(default = NonZeroUsize::MIN)]
    pub dispatcher_capacity: NonZeroUsize,
    /// Consecutive progress passes before the dispatcher yields.
    #[builder(default = Defaults::FAIRNESS_YIELD_INTERVAL)]
    pub fairness_yield_interval: NonZeroU32,
    /// Dispatcher park duration when the broadcast has no work.
    #[builder(default = Duration::from_millis(100))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub idle_timeout: Duration,
    /// Threshold for reporting a slow packager tick.
    #[builder(default = Duration::from_millis(10))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub slow_tick_threshold: Duration,
    /// Maximum consecutive packager ticks in one dispatcher visit.
    #[builder(default = NonZeroU32::MIN)]
    pub task_burst: NonZeroU32,
    /// Dispatcher wait duration between deferred RT wakes.
    #[builder(default = Duration::from_millis(2))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub wait_timeout: Duration,
    /// Packager task priority. Not a document key: `Priority` carries no
    /// `Deserialize`, and giving `kithara-worker` one for a knob nobody has
    /// asked to tune widens that crate's surface for nothing.
    #[builder(default = Priority::new(0))]
    #[patch(skip)]
    pub priority: Priority,
    /// Maximum compute jobs admitted for the packager task.
    #[builder(default = NonZeroUsize::MIN)]
    pub max_compute_tasks: NonZeroUsize,
    /// Maximum time a graceful stop waits for the bounded PCM tail.
    #[builder(default = Duration::from_secs(10))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub stop_timeout: Duration,
}

struct Defaults;

impl Defaults {
    const BUFFER_FRAMES: NonZeroUsize = match NonZeroUsize::new(96_000) {
        Some(value) => value,
        None => unreachable!(),
    };
    const FAIRNESS_YIELD_INTERVAL: NonZeroU32 = match NonZeroU32::new(16) {
        Some(value) => value,
        None => unreachable!(),
    };
    const GENERATION_CAPACITY: NonZeroUsize = match NonZeroUsize::new(8) {
        Some(value) => value,
        None => unreachable!(),
    };
    const TICK_FRAMES: NonZeroUsize = match NonZeroUsize::new(4_096) {
        Some(value) => value,
        None => unreachable!(),
    };
}

impl<S> Clone for BroadcastConfig<S> {
    fn clone(&self) -> Self {
        Self {
            worker: self.worker.clone(),
            pools: self.pools.clone(),
            cancel: self.cancel.clone(),
            segment_target: self.segment_target,
            bind: self.bind,
            channels: self.channels,
            sample_rate: self.sample_rate,
            bit_rate: self.bit_rate,
            codec: self.codec,
            container: self.container,
            grace: self.grace,
            window: self.window,
            buffer_frames: self.buffer_frames,
            tick_frames: self.tick_frames,
            generation_capacity: self.generation_capacity,
            dispatcher_capacity: self.dispatcher_capacity,
            fairness_yield_interval: self.fairness_yield_interval,
            idle_timeout: self.idle_timeout,
            slow_tick_threshold: self.slow_tick_threshold,
            task_burst: self.task_burst,
            wait_timeout: self.wait_timeout,
            priority: self.priority,
            max_compute_tasks: self.max_compute_tasks,
            stop_timeout: self.stop_timeout,
        }
    }
}

impl<S> fmt::Debug for BroadcastConfig<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastConfig")
            .field("pools", &self.pools)
            .field("cancel", &self.cancel.is_some())
            .field("segment_target", &self.segment_target)
            .field("bind", &self.bind)
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .field("bit_rate", &self.bit_rate)
            .field("codec", &self.codec)
            .field("container", &self.container)
            .field("grace", &self.grace)
            .field("window", &self.window)
            .field("buffer_frames", &self.buffer_frames)
            .field("tick_frames", &self.tick_frames)
            .field("generation_capacity", &self.generation_capacity)
            .field("dispatcher_capacity", &self.dispatcher_capacity)
            .field("fairness_yield_interval", &self.fairness_yield_interval)
            .field("idle_timeout", &self.idle_timeout)
            .field("slow_tick_threshold", &self.slow_tick_threshold)
            .field("task_burst", &self.task_burst)
            .field("wait_timeout", &self.wait_timeout)
            .field("priority", &self.priority)
            .field("max_compute_tasks", &self.max_compute_tasks)
            .field("stop_timeout", &self.stop_timeout)
            .finish_non_exhaustive()
    }
}

impl<S> BroadcastConfig<S> {
    const MILLIS_PER_SECOND: u64 = 1_000;

    const MIN_TARGETS: u64 = 3;

    /// Copy this configuration with the measured master sample rate.
    #[must_use]
    pub fn with_sample_rate(&self, sample_rate: u32) -> Self {
        Self {
            sample_rate,
            ..self.clone()
        }
    }

    pub(crate) fn target_seconds(&self) -> BroadcastResult<u64> {
        Ok(self.target_ticks()?.div_ceil(u64::from(self.sample_rate)))
    }

    pub(crate) fn target_ticks(&self) -> BroadcastResult<u64> {
        u64::try_from(self.segment_target.as_millis())
            .ok()
            .and_then(|millis| millis.checked_mul(u64::from(self.sample_rate)))
            .map(|ticks| ticks / Self::MILLIS_PER_SECOND)
            .filter(|ticks| *ticks > 0)
            .ok_or(BroadcastError::InvalidConfig {
                field: "segment_target",
            })
    }

    pub(crate) fn validate(&self) -> BroadcastResult<()> {
        if self.codec != AudioCodec::AacLc || self.container != ContainerFormat::Adts {
            return Err(BroadcastError::UnsupportedProfile {
                codec: self.codec,
                container: self.container,
            });
        }
        if self.sample_rate == 0 {
            return Err(BroadcastError::InvalidConfig {
                field: "sample_rate",
            });
        }
        if self.channels == 0 {
            return Err(BroadcastError::InvalidConfig { field: "channels" });
        }
        if self.bit_rate == 0 {
            return Err(BroadcastError::InvalidConfig { field: "bit_rate" });
        }
        if self.window == 0 {
            return Err(BroadcastError::InvalidConfig { field: "window" });
        }
        if self.stop_timeout.is_zero() {
            return Err(BroadcastError::InvalidConfig {
                field: "stop_timeout",
            });
        }

        let window = u64::try_from(self.window)
            .map_err(|_| BroadcastError::InvalidConfig { field: "window" })?;
        let span_ts = window
            .checked_mul(self.target_ticks()?)
            .ok_or(BroadcastError::InvalidConfig { field: "window" })?;
        let minimum_ts = Self::MIN_TARGETS * self.target_seconds()? * u64::from(self.sample_rate);
        if span_ts < minimum_ts {
            return Err(BroadcastError::PlaylistTooShort {
                span_ts,
                minimum_ts,
                window: self.window,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::testing::{TestPools, pools};
    use kithara_platform::time::Duration;
    use kithara_stream::{AudioCodec, ContainerFormat};
    use kithara_test_utils::kithara;
    use kithara_worker::{Worker, WorkerConfig};

    use super::{BroadcastConfig, BroadcastConfigPatch, BroadcastError};

    fn config() -> BroadcastConfig<TestPools> {
        BroadcastConfig::builder(Worker::new(WorkerConfig::new()), pools()).build()
    }

    #[kithara::test(native, flash(false))]
    fn the_default_configuration_serves_a_long_enough_playlist() {
        config()
            .validate()
            .expect("the defaults hold the RFC 8216 live window");
    }

    #[kithara::test(native, flash(false))]
    fn a_window_shorter_than_three_target_durations_is_rejected() {
        let mut short = config();
        short.segment_target = Duration::from_millis(500);
        short.window = 5;

        short
            .validate()
            .expect_err("five 500 ms segments span 2.5 s of a 1 s target duration");
        short.window = 6;
        short
            .validate()
            .expect("six of them span exactly three target durations");
    }

    #[kithara::test(native, flash(false))]
    fn the_target_duration_rounds_the_segment_target_up_to_seconds() {
        let mut config = config();
        config.segment_target = Duration::from_millis(1_500);

        assert_eq!(config.target_seconds().expect("seconds"), 2);
        assert_eq!(config.target_ticks().expect("ticks"), 72_000);
    }

    #[kithara::test(native, flash(false))]
    fn zero_audio_is_rejected() {
        let mut invalid = config();
        invalid.sample_rate = 0;
        assert!(invalid.validate().is_err());
        invalid.sample_rate = 48_000;
        invalid.channels = 0;
        assert!(invalid.validate().is_err());
        invalid.channels = 2;
        invalid.segment_target = Duration::ZERO;
        assert!(invalid.validate().is_err());
    }

    #[kithara::test(native, flash(false))]
    fn measured_rate_preserves_the_selected_profile() {
        let configured = BroadcastConfig::builder(Worker::new(WorkerConfig::new()), pools())
            .sample_rate(44_100)
            .codec(AudioCodec::AacLc)
            .container(ContainerFormat::Adts)
            .bit_rate(192_000)
            .build();

        let measured = configured.with_sample_rate(48_000);

        assert_eq!(measured.sample_rate, 48_000);
        assert_eq!(measured.codec, configured.codec);
        assert_eq!(measured.container, configured.container);
        assert_eq!(measured.bit_rate, configured.bit_rate);
    }

    #[kithara::test(native, flash(false))]
    fn a_patch_writes_the_bit_rate_and_keeps_the_seeded_channel_count() {
        let settings: BroadcastConfigPatch =
            serde_yaml_ng::from_str("bit_rate: 256000\n").expect("the document types");
        let mut config = BroadcastConfig::builder(Worker::new(WorkerConfig::new()), pools())
            .channels(4)
            .build();

        config.apply(settings);

        assert_eq!(config.bit_rate, 256_000);
        assert_eq!(
            config.channels, 4,
            "a document naming only bit_rate must not reset the seeded channel count"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_document_can_compose_a_playlist_the_builder_would_have_refused() {
        let settings: BroadcastConfigPatch =
            serde_yaml_ng::from_str("window: 1\n").expect("the document types");
        let mut config = config();

        config.apply(settings);

        assert!(matches!(
            config.validate(),
            Err(BroadcastError::PlaylistTooShort { .. })
        ));
    }
}
