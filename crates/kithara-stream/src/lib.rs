//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - `Reader`: sync `Read + Seek` via direct Source calls
//! - `dl::Downloader`: unified download orchestrator (owns `HttpClient`,
//!   dispatches `FetchCmd` with per-chunk writer callbacks)

#![forbid(unsafe_code)]

pub mod dl;
mod error;
mod hooks;
mod media;
mod preroll;
mod source;
mod stream;
mod timeline;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use error::{SourceError, StreamError, StreamResult};
pub use hooks::{DecoderHooks, ReaderChunkSignal, ReaderSeekSignal, SharedHooks};
pub use media::{AudioCodec, ContainerFormat, MediaInfo};
pub use preroll::PrerollHint;
pub use source::{
    NotReadyCause, PendingReason, ReadOutcome, SegmentDescriptor, SegmentLayout, Source,
    SourcePhase, SourceSeekAnchor,
};
pub use stream::{
    Stream, StreamPending, StreamReadError, StreamReadOutcome, StreamSeekPastEof, StreamType,
    VariantChangeError,
};
pub use timeline::{ChunkPosition, Timeline};
