//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - Provide async `Source` trait for random-access byte reading.
//! - `SyncReader` adapter for sync `Read + Seek` with any `SyncSource`.
//! - Writer supports user-defined event mapping per chunk.

#![forbid(unsafe_code)]

mod audio_stream;
mod error;
mod facade;
mod media_info;
mod msg;
mod pipe;
mod source;

pub use audio_stream::{Stream, StreamConfig, StreamType};
pub use error::{StreamError, StreamResult};
pub use facade::{OpenResult, OpenedSource, SourceFactory};
pub use kithara_storage::WaitOutcome;
pub use media_info::{AudioCodec, ContainerFormat, MediaInfo};
pub use msg::StreamMsg;
pub use pipe::{Reader, ReaderError, Writer, WriterError};
pub use source::{Source, SyncReader, SyncSource};
