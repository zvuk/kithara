//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - Generic over in-band `Control` and `Event` message types (defined by higher-level crates).
//! - Deadlock-free orchestration of a *writer* (fetch → storage) and a *reader* (storage → consumer).
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
mod fetch;
mod legacy;
mod msg;

pub use engine::{Engine, EngineCommand, EngineHandle, EngineSource, EngineStream, WriterTask};
pub use error::StreamError;
pub use fetch::{
    Net, ReadSource, Reader, ReaderError, WaitOutcome, WriteSink, Writer, WriterError,
};
// Legacy surface (temporary)
pub use legacy::{ByteStream, Command, Handle, Source, SourceStream, Stream};
pub use msg::{Message, StreamMsg, StreamParams};
