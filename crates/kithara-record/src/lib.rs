#![forbid(unsafe_code)]

//! Storage-neutral recording over Kithara's continuous encoder sessions.

mod config;
mod core;
mod error;
mod live;
mod sink;

pub use core::RecordingCore;

pub use config::{LiveRecordingConfig, RecordingConfig};
pub use error::{LiveRecordingError, RecordingError, RecordingResult};
pub use live::{LiveRecorder, LiveRecordingHandle, LiveRecordingReport, RecordingOutput};
pub use sink::{PartSinkFactory, RecordingSink};
mod consts;
