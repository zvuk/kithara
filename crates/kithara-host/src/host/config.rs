use std::{marker::PhantomData, num::NonZeroU32};

use kithara_effects::LimiterConfig;
#[cfg(feature = "offline")]
use {
    kithara_bufpool::PoolRegion,
    kithara_platform::time::Duration,
    kithara_worker::{DispatcherConfig, TaskConfig, WorkerConfig},
};

const DEFAULT_SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
    Some(sample_rate) => sample_rate,
    None => unreachable!(),
};

/// Configuration for the shared output session owned by `Host`.
#[kithara_config::config(construction, builder = false)]
#[cfg_attr(not(feature = "offline"), derive_where::derive_where(Clone, Copy))]
#[non_exhaustive]
pub enum HostConfig<S> {
    /// Device-backed platform session.
    #[config(sdk)]
    #[non_exhaustive]
    Realtime {
        /// Initial device sample-rate hint in hertz; `Host::set_sample_rate` moves it later.
        #[config(value)]
        sample_rate_hint: NonZeroU32,
        /// Optional native output callback size in frames. `None` preserves the backend default.
        #[config(value)]
        output_block_frames: Option<NonZeroU32>,
        /// Session output limiter policy prepared when the host starts.
        #[config(nested)]
        limiter: LimiterConfig,
        #[config(skip = "type marker")]
        marker: PhantomData<fn() -> S>,
    },
    /// Device-free finite renderer.
    #[cfg(feature = "offline")]
    #[non_exhaustive]
    Offline {
        /// Typed output pool shared with the Host's players.
        #[config(skip = "injected output pool")]
        pools: PoolRegion<S>,
        /// Initial offline output rate; `Host::set_sample_rate` moves it later.
        #[config(value)]
        sample_rate: NonZeroU32,
        /// Maximum frames processed by one backend/task quantum.
        #[config(value)]
        max_block_frames: NonZeroU32,
        /// Firewheel smoothing window for graph changes.
        #[config(value)]
        declick_frames: NonZeroU32,
        /// Declared device-equivalent latency used by transport calculations.
        #[config(value)]
        declared_latency: Duration,
        /// Session output limiter policy.
        #[config(nested)]
        limiter: LimiterConfig,
        /// Shared worker configuration for the session scheduler.
        #[config(nested)]
        worker: WorkerConfig,
        /// Dispatcher budgets for the single offline session task.
        #[config(nested)]
        dispatcher: Box<DispatcherConfig>,
        /// Admission, priority, and cancellation configuration for the session task.
        #[config(nested)]
        task: TaskConfig,
    },
}

#[bon::bon]
impl<S> HostConfig<S> {
    /// Configure a platform realtime session.
    #[builder(
        builder_type(vis = "pub"),
        state_mod(vis = "pub"),
        start_fn(name = builder, vis = "pub")
    )]
    fn new(
        #[builder(default = DEFAULT_SAMPLE_RATE)] sample_rate_hint: NonZeroU32,
        output_block_frames: Option<NonZeroU32>,
        #[builder(default)] limiter: LimiterConfig,
    ) -> Self {
        Self::Realtime {
            sample_rate_hint,
            output_block_frames,
            limiter,
            marker: PhantomData,
        }
    }

    /// Initial sample rate requested by the selected session mode.
    #[must_use]
    pub const fn sample_rate(&self) -> NonZeroU32 {
        match self {
            Self::Realtime {
                sample_rate_hint, ..
            } => *sample_rate_hint,
            #[cfg(feature = "offline")]
            Self::Offline { sample_rate, .. } => *sample_rate,
        }
    }
}
