//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - `Reader`: sync `Read + Seek` via direct Source calls
//! - `Writer`: async HTTP download as `Stream` trait
//! - `Backend`: spawns Downloader task, holds Source for direct access

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

mod backend;
mod downloader;
mod error;
mod fetch;
mod media;
mod reader;
mod source;
mod stream;
mod writer;

pub use backend::Backend;
pub use downloader::{Downloader, NoDownload};
pub use error::{StreamError, StreamResult};
pub use fetch::{EpochValidator, Fetch};
pub use kithara_storage::WaitOutcome;
pub use media::{AudioCodec, ContainerFormat, MediaInfo};
pub use reader::Reader;
pub use source::Source;
// Test utilities
#[cfg(any(test, feature = "test-utils"))]
pub use source::SourceMock;
pub use stream::{Stream, StreamType};
#[cfg(any(test, feature = "test-utils"))]
pub use unimock;
pub use writer::{NetWriter, Writer, WriterError, WriterItem};
