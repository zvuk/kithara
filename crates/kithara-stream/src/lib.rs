//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - Generic over in-band `Control` and `Event` message types (defined by higher-level crates).
//! - Deadlock-free orchestration of a *writer* (fetch → storage) and a *reader* (storage → consumer).
//! - Writer supports user-defined event mapping per chunk (generic `Ev`), suitable for `StreamMsg::Event`.
//! - Provide an async `Stream` API as the primary surface.
//!
//! ## Transitional note
//! This crate previously contained a legacy `Source`/`Stream` orchestrator in a single `lib.rs`.
//! During the migration we keep *legacy* types available behind re-exports while introducing a
//! new `Engine` / `EngineSource` API. Higher-level crates are expected to move to `Engine`.
//!
//! The crate is split into modules; `lib.rs` only re-exports.

#![forbid(unsafe_code)]

mod engine;
mod error;
mod facade;
mod msg;
mod pipe;
mod prefetch;
mod source;
mod stream_source;

pub use engine::{Engine, EngineCommand, EngineHandle, EngineSource, EngineStream, WriterTask};
pub use error::{StreamError, StreamResult};
pub use facade::{OpenedSource, SourceFactory};
pub use kithara_storage::WaitOutcome;
pub use msg::{EngineParams, StreamMsg};
pub use pipe::{Reader, ReaderError, Writer, WriterError};
pub use prefetch::{
    PrefetchConsumer, PrefetchResult, PrefetchSource, PrefetchWorker, PrefetchedItem,
};
pub use source::{Source, SyncReader, SyncReaderParams};
pub use stream_source::StreamSource;
