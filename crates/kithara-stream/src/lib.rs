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
mod playhead;
mod preroll;
mod seek_state;
mod source;
mod stream;
mod timeline;
mod wake;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use error::{SourceError, StreamError, StreamResult};
pub use hooks::{BoxedEventSink, ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal};
pub use media::{AudioCodec, ContainerFormat, MediaInfo};
pub use playhead::{PlayheadRead, PlayheadWrite};
pub use preroll::PrerollHint;
pub use seek_state::{Activity, SeekControl, SeekObserve};
pub use source::{
    NotReadyCause, PendingReason, ReadOutcome, SegmentDescriptor, SegmentLayout, Source,
    SourcePhase, SourceSeekAnchor,
};
pub use stream::{
    Stream, StreamPending, StreamReadError, StreamReadOutcome, StreamSeekPastEof, StreamType,
    VariantChangeError,
};
pub use timeline::{ChunkPosition, Timeline};
pub use wake::DeferredWake;
