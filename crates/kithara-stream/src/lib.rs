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
mod fetch;
mod io;
mod msg;

pub use engine::{Engine, EngineCommand, EngineHandle, EngineSource, EngineStream, WriterTask};
pub use error::{StreamError, StreamResult};
pub use fetch::{Reader, ReaderError, Writer, WriterError};
pub use io::{Source, SyncReader};
pub use kithara_storage::WaitOutcome;
pub use msg::{StreamMsg, StreamParams};
