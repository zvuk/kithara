#![forbid(unsafe_code)]

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

// ── Re-export sub-crates ────────────────────────────────────────────────

pub mod audio {
    pub use kithara_audio::*;
}

pub mod decode {
    pub use kithara_decode::*;
}

pub mod stream {
    pub use kithara_stream::*;
}

#[cfg(feature = "file")]
pub mod file {
    pub use kithara_file::*;
}

#[cfg(feature = "hls")]
pub mod hls {
    pub use kithara_hls::*;
}

#[cfg(feature = "hls")]
pub mod abr {
    pub use kithara_abr::*;
}

#[cfg(feature = "assets")]
pub mod assets {
    pub use kithara_assets::*;
}

#[cfg(feature = "net")]
pub mod net {
    pub use kithara_net::*;
}

#[cfg(feature = "bufpool")]
pub mod bufpool {
    pub use kithara_bufpool::*;
}

// ── Resource ────────────────────────────────────────────────────────────

mod config;
mod events;
mod resource;
#[cfg(feature = "rodio")]
mod rodio_impl;
mod source_type;

pub use config::{ResourceConfig, ResourceSrc};
pub use events::ResourceEvent;
pub use resource::Resource;
pub use source_type::SourceType;

// ── Prelude ─────────────────────────────────────────────────────────────

pub mod prelude {
    pub use kithara_audio::{Audio, AudioConfig, PcmReader};
    pub use kithara_decode::{DecodeError, DecodeResult, PcmSpec, TrackMetadata};
    #[cfg(feature = "file")]
    pub use kithara_file::{File, FileConfig};
    #[cfg(feature = "hls")]
    pub use kithara_hls::{Hls, HlsConfig};
    pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Stream, StreamType};

    pub use crate::{Resource, ResourceConfig, ResourceEvent};
}
