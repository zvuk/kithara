#![forbid(unsafe_code)]

//! # Kithara
//!
//! Facade crate providing a unified API for audio streaming and decoding.
//!
//! ## Quick start
//!
//! ```ignore
//! use kithara::{
//!     assets::AssetStore,
//!     bufpool::{OverallBudget, PoolConfig, pool_schema},
//!     prelude::*,
//! };
//!
//! pool_schema! {
//!     AppPools {
//!         bytes: u8,
//!         samples: f32,
//!     }
//! }
//! let pool_config = || PoolConfig::builder().max_buffers(128).build();
//! let pools = AppPools::builder(OverallBudget(64 * 1024 * 1024))
//!     .bytes(pool_config())
//!     .samples(pool_config())
//!     .build()?;
//! let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
//! let config: ResourceConfig<AppPools> =
//!     ResourceConfig::for_src(ResourceSrc::parse("https://example.com/song.mp3")?)
//!         .store(AssetStore::builder(pools).build())
//!         .worker(worker)
//!         .build();
//! let mut resource = Resource::new(config).await?;
//!
//! // Read interleaved PCM
//! let mut buf = [0.0f32; 1024];
//! resource.read(&mut buf);
//! ```

#[cfg(feature = "audio")]
pub mod audio {
    pub use kithara_audio::*;
}

#[cfg(feature = "analysis")]
pub mod analysis {
    pub use kithara_analysis::*;
}

#[cfg(feature = "broadcast")]
pub mod broadcast {
    pub use kithara_broadcast::*;
}

#[cfg(feature = "bufpool")]
pub mod bufpool {
    pub use kithara_bufpool::*;
}

#[cfg(feature = "decode")]
pub mod decode {
    pub use kithara_decode::*;
}

#[cfg(feature = "encode")]
pub mod encode {
    pub use kithara_encode::*;
}

#[cfg(feature = "output")]
pub mod output {
    pub use kithara_output::*;
}

#[cfg(feature = "record")]
pub mod record {
    pub use kithara_record::*;
}

#[cfg(feature = "events")]
pub mod events {
    pub use kithara_events::*;
}

#[cfg(feature = "host")]
pub mod host {
    pub use kithara_host::*;
}

#[cfg(feature = "platform")]
pub mod platform {
    pub use kithara_platform::*;
}

#[cfg(feature = "play")]
pub mod play {
    pub use kithara_play::*;
}

#[cfg(feature = "resampler")]
pub mod resampler {
    pub use kithara_resampler::*;
}

#[cfg(feature = "signal")]
pub mod signal {
    pub use kithara_signal::*;
}

#[cfg(feature = "queue")]
pub mod queue {
    pub use kithara_queue::*;
}

#[cfg(feature = "stream")]
pub mod stream {
    pub use kithara_stream::*;
}

#[cfg(feature = "stretch")]
pub mod stretch {
    pub use kithara_stretch::*;
}

#[cfg(feature = "ui")]
pub mod ui {
    pub use kithara_ui::*;
}

#[cfg(feature = "warp")]
pub mod warp {
    pub use kithara_warp::*;
}

#[cfg(feature = "worker")]
pub mod worker {
    pub use kithara_worker::*;
}

#[cfg(feature = "file")]
pub mod file {
    pub use kithara_file::*;
}

#[cfg(feature = "abr")]
pub mod abr {
    pub use kithara_abr::*;
}

#[cfg(feature = "drm")]
pub mod drm {
    pub use kithara_drm::*;
}

#[cfg(feature = "hls")]
pub mod hls {
    pub use kithara_hls::*;
}

#[cfg(feature = "assets")]
pub mod assets {
    pub use kithara_assets::*;
}

#[cfg(feature = "net")]
pub mod net {
    pub use kithara_net::*;
}

#[cfg(feature = "storage")]
pub mod storage {
    pub use kithara_storage::*;
}

#[cfg(feature = "test-utils")]
pub use kithara_test_utils::{kithara::mock, no_block};
#[cfg(feature = "probe")]
pub use kithara_test_utils::{
    kithara::{fixture, test},
    kithara_facade::{allow_block, flash, no_block},
};
#[cfg(all(
    feature = "warp",
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use kithara_warp::StretchKind;
#[cfg(feature = "warp")]
pub use kithara_warp::{GridSegment, RegionPlan, RegionPlanError, StretchControls};

#[cfg(feature = "mock")]
pub mod mock {
    #[cfg(feature = "audio")]
    pub use kithara_audio::mock::*;
    #[cfg(feature = "decode")]
    pub use kithara_decode::mock::*;
    #[cfg(feature = "play")]
    pub use kithara_play::mock::*;
    #[cfg(feature = "stream")]
    pub use kithara_stream::mock::*;
}

/// Prelude — flat imports for common types.
pub mod prelude {
    #[cfg(feature = "abr")]
    pub use kithara_abr::AbrMode;
    #[cfg(feature = "audio")]
    pub use kithara_audio::{
        Audio, AudioConfig, AudioControl, AudioRead, AudioReader, AudioSession, ResamplerQuality,
    };
    #[cfg(feature = "decode")]
    pub use kithara_decode::{DecodeError, DecodeResult, DecoderTrackInfo, TrackMetadata};
    #[cfg(feature = "events")]
    pub use kithara_events::HlsEvent;
    #[cfg(feature = "events")]
    pub use kithara_events::{AudioEvent, BusScope, Event, EventBus, EventReceiver, FileEvent};
    #[cfg(feature = "file")]
    pub use kithara_file::{File, FileConfig};
    #[cfg(feature = "hls")]
    pub use kithara_hls::{Hls, HlsConfig};
    #[cfg(feature = "play")]
    pub use kithara_play::{
        EngineConfig, EngineImpl, EngineLoadSnapshot, PlayWorker, PlayWorkerConfig,
        PlaybackResamplerBackend, PlayerConfig, PlayerImpl, Resource, ResourceConfig, ResourceSrc,
        ServiceClass, SourceType,
    };
    #[cfg(feature = "signal")]
    pub use kithara_signal::{AudioChunkInfo, AudioSpec};
    #[cfg(feature = "stream")]
    pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Stream, StreamType};
    #[cfg(all(
        feature = "warp",
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    pub use kithara_warp::StretchKind;
    #[cfg(feature = "warp")]
    pub use kithara_warp::{GridSegment, RegionPlan, RegionPlanError, StretchControls};
}
