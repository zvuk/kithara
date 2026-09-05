#![forbid(unsafe_code)]

//! File streaming implementation for progressive HTTP downloads.

mod config;
mod coord;
mod error;
mod session;
mod stream;
pub use config::{FileConfig, FileConfigPatch, FileSrc};
#[cfg(test)]
pub(crate) use kithara_bufpool::testing as test_pools;
pub use stream::File;
