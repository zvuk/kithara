#![forbid(unsafe_code)]

//! File streaming implementation for progressive HTTP downloads.
//!
//! # Example
//!
//! ```ignore
//! use kithara_stream::Stream;
//! use kithara_file::{File, FileParams};
//! use kithara_assets::StoreOptions;
//!
//! let params = FileParams::new(StoreOptions::new("/tmp/cache"));
//! let stream = Stream::<File>::open(url, params).await?;
//! let events = stream.events();  // Receiver<FileEvent>
//! ```

mod driver;
mod events;
mod options;
mod session;
mod source;

pub use driver::{DriverError, SourceError};
pub use events::FileEvent;
pub use options::{FileParams, FileSourceOptions, OptionsError};
pub use session::{FileError, FileResult, FileSession, Progress, SessionSource};
pub use source::{File, FileSource, FileSourceContract};
