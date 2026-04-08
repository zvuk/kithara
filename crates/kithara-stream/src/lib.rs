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
mod cursor;
mod demand;
pub mod dl;
mod error;
mod fetch;
mod layout;
mod media;
mod media_rfc6381;
mod source;
mod stream;
mod timeline;
mod topology;
mod writer;

#[cfg(feature = "internal")]
pub mod internal;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use context::{NullStreamContext, StreamContext};
pub use coordination::TransferCoordination;
pub use cursor::DownloadCursor;
pub use demand::DemandSlot;
pub use error::{StreamError, StreamResult};
pub use fetch::{EpochValidator, Fetch};
pub use layout::LayoutIndex;
pub use media::{AudioCodec, ContainerFormat, MediaInfo};
pub use media_rfc6381::{MediaInfoRfc6381Ext, audio_codec_supports_fmp4_packaging};
pub use source::{ReadOutcome, Source, SourcePhase, SourceSeekAnchor, VariantChangeError};
pub use stream::{Stream, StreamType};
pub use timeline::Timeline;
pub use topology::Topology;
pub use writer::{Writer, WriterError, WriterItem};

/// Plan outcome type — kept for HLS compatibility until Phase 4.
pub enum PlanOutcome<P> {
    Batch(Vec<P>),
    Step,
    Complete,
    Idle,
}
