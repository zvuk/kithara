//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - `Reader`: sync `Read + Seek` via direct Source calls

#![forbid(unsafe_code)]

mod activity;
mod consts;
mod error;
mod hooks;
mod media;
mod playhead;
mod preroll;
mod profile;
mod reader;
#[cfg(test)]
mod seek_state;
mod source;
mod stream;
mod transition;
mod wake;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use activity::{Activity, ActivityWriter};
pub use error::{SourceError, StreamError, StreamResult};
pub use hooks::{BoxedEventSink, ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal};
pub use kithara_storage::WaitOutcome;
pub use media::{AudioCodec, ContainerFormat, MediaInfo, id3v2_tag_len, needs_exact_byte_sizes};
pub use playhead::{ChunkPosition, PlayheadRead, PlayheadState, PlayheadWrite};
pub use preroll::PrerollHint;
pub use profile::{ReaderInput, ReaderProfile, ReaderWarmup};
pub use reader::{
    ConstructionGate, OpenedReader, OpenedVariantReader, SessionReader, VariantReaderPlan,
    VariantReaderTake,
};
pub use source::{
    ByteMap, NotReadyCause, PendingReason, ReadOutcome, SegmentDescriptor, Source, SourcePhase,
    SourceProbe, SourceSeekAnchor, VariantControl,
};
pub use stream::{
    Stream, StreamPending, StreamReadError, StreamReadOutcome, StreamSeekPastEof, StreamType,
    VariantChangeError, format_change_segment_range, resolve_seek_target,
};
pub use transition::{
    OutgoingDisposition, VariantPromotion, VariantTransition, VariantTransitionId,
};
pub use wake::{DeferredWake, WorkerWake};
