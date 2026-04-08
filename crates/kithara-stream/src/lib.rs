//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - `Reader`: sync `Read + Seek` via direct Source calls
//! - `Writer`: async HTTP download as `Stream` trait
//! - `dl::Downloader`: unified download orchestrator

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

mod context;
mod coordination;
mod demand;
pub mod dl;
mod error;
mod fetch;
mod media;
mod media_rfc6381;
mod source;
mod stream;
mod timeline;
mod writer;

#[cfg(feature = "internal")]
pub mod internal;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use context::{NullStreamContext, StreamContext};
pub use coordination::TransferCoordination;
pub use demand::DemandSlot;
pub use error::{StreamError, StreamResult};
pub use fetch::{EpochValidator, Fetch};
pub use media::{AudioCodec, ContainerFormat, MediaInfo};
pub use media_rfc6381::audio_codec_supports_fmp4_packaging;
pub use source::{ReadOutcome, Source, SourcePhase, SourceSeekAnchor, VariantChangeError};
pub use stream::{Stream, StreamType};
pub use timeline::Timeline;
pub use writer::{Writer, WriterError, WriterItem};
