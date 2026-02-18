#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

//! # Kithara
//!
//! Facade crate providing a unified API for audio streaming and decoding.
//!
//! ## Quick start
//!
//! ```ignore
//! use kithara::prelude::*;
//!
//! // Auto-detect from URL
//! let config = ResourceConfig::new("https://example.com/song.mp3")?;
//! let mut resource = Resource::new(config).await?;
//!
//! // Read interleaved PCM
//! let mut buf = [0.0f32; 1024];
//! resource.read(&mut buf);
//! ```

// Virtual modules — namespaced access to all subcrates.

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

// Mock module — re-exports mocks from all subcrates with test-utils.
#[cfg(feature = "test-utils")]
pub mod mock {
    pub use kithara_audio::mock::*;
    pub use kithara_decode::mock::*;
    pub use kithara_play::mock::*;
    pub use kithara_stream::mock::*;
}

/// Prelude — flat imports for common types.
pub mod prelude {
    // Audio pipeline
    // HLS (optional)
    #[cfg(feature = "hls")]
    pub use kithara_abr::{AbrMode, AbrOptions};
    pub use kithara_audio::{Audio, AudioConfig, PcmReader, ResamplerQuality};
    // Decode
    pub use kithara_decode::{DecodeError, DecodeResult, PcmMeta, PcmSpec, TrackMetadata};
    // Events
    #[cfg(feature = "hls")]
    pub use kithara_events::HlsEvent;
    pub use kithara_events::{AudioEvent, Event, EventBus, FileEvent};
    // File (optional)
    #[cfg(feature = "file")]
    pub use kithara_file::{File, FileConfig};
    #[cfg(feature = "hls")]
    pub use kithara_hls::{Hls, HlsConfig};
    // Platform
    pub use kithara_platform::ThreadPool;
    // Play
    pub use kithara_play::{
        EngineConfig, EngineImpl, PlayerConfig, PlayerImpl, Resource, ResourceConfig, ResourceSrc,
        SourceType,
    };
    // Stream
    pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Stream, StreamType};
}
