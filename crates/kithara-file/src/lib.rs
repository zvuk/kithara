#![forbid(unsafe_code)]

//! File streaming implementation for progressive HTTP downloads.
//!
//! # Example
//!
//! ```ignore
//! use kithara_stream::{Stream, StreamType};
//! use kithara_file::{File, FileConfig, FileParams};
//!
//! // Using StreamType API
//! let config = FileConfig::new(url).with_params(FileParams::default());
//! let inner = File::create(config).await?;
//! ```

mod error;
mod events;
mod inner;
mod options;
mod session;
mod source;

pub use error::SourceError;
pub use events::FileEvent;
pub use inner::{File, FileConfig, FileInner};
pub use options::FileParams;
pub use session::{Progress, SessionSource};
