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
mod context;
mod downloader;
mod error;
mod fetch;
mod media;
mod source;
mod stream;
mod writer;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use backend::Backend;
pub use context::{NullStreamContext, StreamContext};
pub use downloader::{Downloader, DownloaderIo, PlanOutcome, StepResult};
pub use error::{StreamError, StreamResult};
pub use fetch::{EpochValidator, Fetch};
pub use media::{AudioCodec, ContainerFormat, MediaInfo};
pub use source::Source;
pub use stream::{Stream, StreamType};
pub use writer::{Writer, WriterError, WriterItem};
