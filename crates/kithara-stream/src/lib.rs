//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - Generic over in-band `Control` and `Event` message types (defined by higher-level crates).
//! - Provide async `Source` trait for random-access byte reading.
//! - `SyncReader` adapter for sync `Read + Seek` over async `Source`.
//! - Writer supports user-defined event mapping per chunk (generic `Ev`), suitable for `StreamMsg::Event`.
//! - `SourceFactory` pattern for creating sources with unified API.

#![forbid(unsafe_code)]

mod error;
mod facade;
mod media_info;
mod msg;
mod pipe;
mod prefetch;
mod source;
mod stream_message;
mod stream_source;

pub use error::{StreamError, StreamResult};
pub use facade::{OpenResult, OpenedSource, SourceFactory};
pub use kithara_storage::WaitOutcome;
pub use media_info::{AudioCodec, ContainerFormat, MediaInfo};
pub use msg::StreamMsg;
pub use pipe::{Reader, ReaderError, Writer, WriterError};
pub use prefetch::{
    AlwaysValid, BlockingSource, BlockingWorker, EpochValidator, Fetch, ItemValidator,
    PrefetchConsumer, PrefetchResult, PrefetchSource, PrefetchWorker, PrefetchedItem, WorkerItem,
};
pub use source::{Source, SyncReader, SyncReaderParams};
pub use stream_message::{StreamData, StreamMessage, StreamMetadata};
pub use stream_source::StreamSource;
