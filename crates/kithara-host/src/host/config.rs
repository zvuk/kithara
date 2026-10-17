use std::{
    marker::PhantomData,
    num::{NonZeroU16, NonZeroU32, NonZeroUsize},
};

use kithara_command::{ChannelConfig, ScopedConfig};
use kithara_effects::LimiterConfig;
use kithara_render::rt::DeckMixerConfig;
#[cfg(feature = "offline")]
use {
    kithara_bufpool::PoolRegion,
    kithara_platform::time::Duration,
    kithara_worker::{DispatcherConfig, TaskConfig, WorkerConfig},
};

use crate::{HostSettings, consts};

/// Configuration for the shared output session owned by `Host`.
#[cfg_attr(not(feature = "offline"), derive_where::derive_where(Clone, Copy))]
#[non_exhaustive]
pub enum HostConfig<S> {
    /// Device-backed platform session.
    #[non_exhaustive]
    Realtime {
        /// Optional native output callback-size override. `None` preserves the backend default.
        output_block_frames: Option<NonZeroU32>,
        /// Maximum simultaneously open or closing deck scopes. Default: 8.
        max_decks: NonZeroU16,
        /// Batches in flight per deck scope. Default: 32.
        deck_capacity: NonZeroUsize,
        /// Maximum slots an opened deck scope can address.
        max_deck_slots: NonZeroUsize,
        /// Session output limiter policy.
        limiter: LimiterConfig,
        /// Settings the Host starts with; they change while it runs.
        settings: HostSettings,
        marker: PhantomData<fn() -> S>,
    },
    /// Device-free finite renderer.
    #[cfg(feature = "offline")]
    #[non_exhaustive]
    Offline {
        /// Typed output pool shared with the Host's players.
        pools: PoolRegion<S>,
        /// Maximum frames processed by one backend/task quantum.
        max_block_frames: NonZeroU32,
        /// Firewheel smoothing window for graph changes.
        declick_frames: NonZeroU32,
        /// Declared device-equivalent latency used by transport calculations.
        declared_latency: Duration,
        /// Maximum simultaneously open or closing deck scopes. Default: 8.
        max_decks: NonZeroU16,
        /// Batches in flight per deck scope. Default: 32.
        deck_capacity: NonZeroUsize,
        /// Maximum slots an opened deck scope can address.
        max_deck_slots: NonZeroUsize,
        /// Session output limiter policy.
        limiter: LimiterConfig,
        /// Settings the Host starts with; they change while it runs.
        settings: HostSettings,
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
        output_block_frames: Option<NonZeroU32>,
        #[builder(default = consts::MAX_DECKS)] max_decks: NonZeroU16,
        #[builder(default = consts::DECK_CAPACITY)] deck_capacity: NonZeroUsize,
        #[builder(default = DeckMixerConfig::default().slots())] max_deck_slots: NonZeroUsize,
        #[builder(default)] limiter: LimiterConfig,
        #[builder(default)] settings: HostSettings,
    ) -> Self {
        Self::Realtime {
            output_block_frames,
            max_decks,
            deck_capacity,
            max_deck_slots,
            limiter,
            settings,
            marker: PhantomData,
        }
    }

    /// Settings the Host starts with.
    #[must_use]
    pub const fn settings(&self) -> HostSettings {
        match self {
            Self::Realtime { settings, .. } => *settings,
            #[cfg(feature = "offline")]
            Self::Offline { settings, .. } => *settings,
        }
    }

    /// Maximum simultaneously open or closing deck scopes.
    #[must_use]
    pub const fn max_decks(&self) -> NonZeroU16 {
        match self {
            Self::Realtime { max_decks, .. } => *max_decks,
            #[cfg(feature = "offline")]
            Self::Offline { max_decks, .. } => *max_decks,
        }
    }

    /// Batches in flight per deck scope.
    #[must_use]
    pub const fn deck_capacity(&self) -> NonZeroUsize {
        match self {
            Self::Realtime { deck_capacity, .. } => *deck_capacity,
            #[cfg(feature = "offline")]
            Self::Offline { deck_capacity, .. } => *deck_capacity,
        }
    }

    /// Maximum slots an opened deck scope can address.
    #[must_use]
    pub const fn max_deck_slots(&self) -> NonZeroUsize {
        match self {
            Self::Realtime { max_deck_slots, .. } => *max_deck_slots,
            #[cfg(feature = "offline")]
            Self::Offline { max_deck_slots, .. } => *max_deck_slots,
        }
    }

    pub(crate) fn channel_config(&self) -> ScopedConfig {
        ScopedConfig::builder()
            .root(ChannelConfig::builder().build())
            .scopes(self.max_decks())
            .scope(
                ChannelConfig::builder()
                    .capacity(self.deck_capacity())
                    .targets(self.max_deck_slots().get())
                    .build(),
            )
            .build()
    }
}
