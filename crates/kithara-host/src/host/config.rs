use std::{marker::PhantomData, num::NonZeroU32};

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
#[non_exhaustive]
pub enum HostConfig<S> {
    /// Device-backed platform session.
    #[non_exhaustive]
    Realtime {
        /// Initial device-rate hint. Physical route changes may update it later.
        sample_rate_hint: NonZeroU32,
        /// Requested native callback size; defaults to 128 frames. `None` uses the backend default.
        output_block_frames: Option<NonZeroU32>,
        marker: PhantomData<fn() -> S>,
    },
    /// Device-free finite renderer.
    #[cfg(feature = "offline")]
    #[non_exhaustive]
    Offline {
        /// Typed output pool shared with the Host's players.
        pools: PoolRegion<S>,
        /// Exact offline output rate.
        sample_rate: NonZeroU32,
        /// Maximum frames processed by one backend/task quantum.
        max_block_frames: NonZeroU32,
        /// Firewheel smoothing window for graph changes.
        declick_frames: NonZeroU32,
        /// Declared device-equivalent latency used by transport calculations.
        declared_latency: Duration,
        /// Shared worker configuration for the session scheduler.
        worker: WorkerConfig,
        /// Dispatcher budgets for the single offline session task.
        dispatcher: Box<DispatcherConfig>,
        /// Admission, priority, and cancellation configuration for the session task.
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
        #[builder(required, with = Some, default = NonZeroU32::new(128))]
        output_block_frames: Option<NonZeroU32>,
    ) -> Self {
        Self::Realtime {
            sample_rate_hint,
            output_block_frames,
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
