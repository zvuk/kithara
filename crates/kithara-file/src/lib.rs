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

mod error;
mod events;
mod inner;
mod options;
mod session;

pub use error::SourceError;
pub use events::FileEvent;
pub use inner::File;
pub use options::FileConfig;
pub use session::Progress;
