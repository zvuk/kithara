//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - `Reader`: sync `Read + Seek` via kanal channel (no block_on)
//! - `Writer`: async HTTP download as `Stream` trait
//! - `Backend`: generic async backend for any `Source`

#![forbid(unsafe_code)]

mod backend;
mod error;
mod media;
mod reader;
mod source;
mod stream;
mod writer;

pub use backend::{Backend, BackendAccess, Command, Response};
pub use error::{StreamError, StreamResult};
pub use kithara_storage::WaitOutcome;
pub use media::{AudioCodec, ContainerFormat, MediaInfo};
pub use reader::Reader;
pub use source::Source;
pub use stream::{Stream, StreamConfig, StreamType};
pub use writer::{Writer, WriterError, WriterItem};
