#![forbid(unsafe_code)]

//! Batch analysis input and execution for the command-line tool.

mod analysis;
mod batch;
mod encode;
mod error;
mod input;
mod pools;
mod sink;

pub use error::RunError;
pub use input::{Args, Batch};
