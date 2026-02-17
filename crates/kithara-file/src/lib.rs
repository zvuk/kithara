#![forbid(unsafe_code)]

//! File streaming implementation for progressive HTTP downloads.
//!
//! # Example
//!
//! ```ignore
//! use kithara_stream::{Stream, StreamType};
//! use kithara_file::{File, FileConfig};
//!
//! // Using StreamType API
//! let config = FileConfig::new(url);
//! let inner = File::create(config).await?;
//! ```

mod config;
mod downloader;
mod error;
mod inner;
mod session;

pub use config::{FileConfig, FileSrc};
#[doc(hidden)]
pub use error::SourceError;
pub use inner::File;
#[doc(hidden)]
pub use session::Progress;
