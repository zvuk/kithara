//! `kithara-stream`
//!
//! Core streaming orchestration primitives for Kithara.
//!
//! ## Design goals
//! - `ChannelReader`: sync `Read + Seek` via kanal channel (no block_on)
//! - `Writer`: async HTTP download as `Stream` trait
//! - `StreamMsg`: generic data + control + event model

#![forbid(unsafe_code)]

mod audio_stream;
mod channel_reader;
mod error;
mod media_info;
mod pipe;
mod source;

pub use audio_stream::{Stream, StreamConfig, StreamType};
pub use channel_reader::{
    BackendCommand, BackendResponse, ChannelBackend, ChannelReader, RandomAccessBackend,
};
pub use error::{StreamError, StreamResult};
pub use kithara_storage::WaitOutcome;
pub use media_info::{AudioCodec, ContainerFormat, MediaInfo};
pub use pipe::{Writer, WriterError, WriterItem};
pub use source::Source;
