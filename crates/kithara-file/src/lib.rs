#![forbid(unsafe_code)]

//! File streaming implementation for progressive HTTP downloads.
//!
//! # Example
//!
//! ```ignore
//! use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
//! use kithara_file::{File, FileParams};
//!
//! // Async source with events
//! let source = StreamSource::<File>::open(url, FileParams::default()).await?;
//! let events = source.events();  // Receiver<FileEvent>
//!
//! // Sync reader for decoders (Read + Seek)
//! let reader = SyncReader::<StreamSource<File>>::open(
//!     url,
//!     FileParams::default(),
//!     SyncReaderParams::default()
//! ).await?;
//! ```

mod error;
mod events;
mod options;
mod session;
mod source;

pub use error::SourceError;
pub use events::FileEvent;
pub use options::FileParams;
pub use session::{Progress, SessionSource};
pub use source::File;
