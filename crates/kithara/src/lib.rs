#![forbid(unsafe_code)]

//! # Kithara
//!
//! Facade crate providing a unified API for audio streaming and decoding.
//!
//! ## Quick start
//!
//! ```ignore
//! use kithara::{bufpool::{BytePool, PcmPool}, prelude::*};
//!
//! // Auto-detect from URL
//! let config = ResourceConfig::new(
//!     "https://example.com/song.mp3",
//!     BytePool::default(),
//!     PcmPool::default(),
//! )?;
//! let mut resource = Resource::new(config).await?;
//!
//! // Read interleaved PCM
//! let mut buf = [0.0f32; 1024];
//! resource.read(&mut buf);
//! ```

pub mod audio {
    pub use kithara_audio::*;
}

pub mod bufpool {
    pub use kithara_bufpool::*;
}

pub mod decode {
    pub use kithara_decode::*;
}

pub mod events {
    pub use kithara_events::*;
}

pub mod platform {
    pub use kithara_platform::*;
}

pub mod play {
    pub use kithara_play::*;
}

pub mod resampler {
    pub use kithara_resampler::*;
}

#[cfg(feature = "queue")]
pub mod queue {
    pub use kithara_queue::*;
}

pub mod stream {
    pub use kithara_stream::*;
}

#[cfg(feature = "file")]
pub mod file {
    pub use kithara_file::*;
}

#[cfg(feature = "hls")]
pub mod abr {
    pub use kithara_abr::*;
}

#[cfg(feature = "hls")]
pub mod drm {
    pub use kithara_drm::*;
}

#[cfg(feature = "hls")]
pub mod hls {
    pub use kithara_hls::*;
}

#[cfg(any(feature = "file", feature = "hls", feature = "assets"))]
pub mod assets {
    pub use kithara_assets::*;
}

#[cfg(any(feature = "file", feature = "hls", feature = "net"))]
pub mod net {
    pub use kithara_net::*;
}

#[cfg(feature = "assets")]
pub mod storage {
    pub use kithara_storage::*;
}

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use kithara_audio::effects::{StretchBackend, StretchBackendError};
pub use kithara_audio::{GridSegment, RegionPlan, RegionPlanError, StretchControls};
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use kithara_audio::{StretchKind, TimeStretchProcessor};
pub use kithara_test_utils::{kithara::mock, no_block};
#[cfg(feature = "probe")]
pub use kithara_test_utils::{
    kithara::{fixture, test},
    kithara_facade::{allow_block, flash, no_block},
};

#[cfg(feature = "mock")]
pub mod mock {
    pub use kithara_audio::mock::*;
    pub use kithara_decode::mock::*;
    pub use kithara_play::mock::*;
    pub use kithara_stream::mock::*;
}

/// Prelude — flat imports for common types.
pub mod prelude {
    #[cfg(feature = "hls")]
    pub use kithara_abr::AbrMode;
    pub use kithara_audio::{
        Audio, AudioConfig, EngineLoadSnapshot, GridSegment, PcmControl, PcmRead, PcmReader,
        PcmSession, RegionPlan, RegionPlanError, ResamplerQuality, StretchControls,
    };
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    pub use kithara_audio::{
        StretchKind, TimeStretchProcessor,
        effects::{StretchBackend, StretchBackendError},
    };
    pub use kithara_decode::{
        DecodeError, DecodeResult, DecoderTrackInfo, PcmMeta, PcmSpec, TrackMetadata,
    };
    #[cfg(feature = "hls")]
    pub use kithara_events::HlsEvent;
    pub use kithara_events::{AudioEvent, BusScope, Event, EventBus, EventReceiver, FileEvent};
    #[cfg(feature = "file")]
    pub use kithara_file::{File, FileConfig};
    #[cfg(feature = "hls")]
    pub use kithara_hls::{Hls, HlsConfig};
    pub use kithara_play::{
        AudioWorkerHandle, EngineConfig, EngineImpl, PlaybackResamplerBackend, PlayerConfig,
        PlayerImpl, Resource, ResourceConfig, ResourceSrc, ServiceClass, SourceType,
    };
    pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Stream, StreamType};
}
