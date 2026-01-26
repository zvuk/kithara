#![forbid(unsafe_code)]

//! File streaming implementation for progressive HTTP downloads.
//!
//! # Example
//!
//! ```ignore
//! use kithara_decode::{MediaSource, StreamDecoder};
//! use kithara_file::{FileMediaSource, FileParams};
//!
//! // Open file media source
//! let source = FileMediaSource::open(url, FileParams::default()).await?;
//! let events = source.events();
//!
//! // Create stream for decoding
//! let stream = source.open()?;
//! let mut decoder = StreamDecoder::new(stream)?;
//!
//! // Decode loop
//! while let Some(chunk) = decoder.decode_next()? {
//!     play_audio(chunk);
//! }
//! ```

mod error;
mod events;
mod media_source;
mod options;
mod session;
mod source;

pub use error::SourceError;
pub use events::FileEvent;
pub use media_source::FileMediaSource;
pub use options::FileParams;
pub use session::{Progress, SessionSource};
pub use source::File;
