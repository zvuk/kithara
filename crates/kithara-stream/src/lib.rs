//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - `Reader`: sync `Read + Seek` via direct Source calls
//! - `dl::Downloader`: unified download orchestrator (owns `HttpClient`,
//!   dispatches `FetchCmd` with per-chunk writer callbacks)

// `deny` (not `forbid`) because `#[kithara::probe]` expansion contains
// `usdt::provider!` which emits inline asm. The macro itself locally
// uses `#[expect(unsafe_code)]` on the generated USDT module; production
// code outside those expansions remains unsafe-free.
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

mod context;
mod demand;
pub mod dl;
mod error;
mod hooks;
mod media;
mod media_rfc6381;
mod source;
mod stream;
mod timeline;

#[cfg(feature = "internal")]
pub mod internal;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use context::{NullStreamContext, StreamContext};
pub use demand::DemandSlot;
pub use error::{SourceError, StreamError, StreamResult};
pub use hooks::{DecoderHooks, ReaderChunkSignal, ReaderSeekSignal, SharedHooks};
pub use media::{AudioCodec, ContainerFormat, MediaInfo};
pub use media_rfc6381::audio_codec_supports_fmp4_packaging;
pub use source::{
    PendingReason, ReadOutcome, SegmentDescriptor, Source, SourcePhase, SourceSeekAnchor,
};
pub use stream::{
    Stream, StreamReadError, StreamReadOutcome, StreamSeekPastEof, StreamType, VariantChangeError,
};
pub use timeline::{ChunkPosition, Timeline};
