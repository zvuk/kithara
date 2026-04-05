//! Unified download orchestrator.
//!
//! [`Downloader`] owns the sole [`HttpClient`](kithara_net::HttpClient) and
//! polls registered protocol streams via
//! [`SelectAll`](futures::stream::SelectAll). Protocols yield
//! [`FetchCmd`] items; the downloader sorts by priority and executes fetches.
//!
//! One `Downloader` instance is shared across all tracks (it is [`Clone`]).
//! New streams can be registered before or after [`spawn`](Downloader::spawn).

mod cmd;
mod config;
mod downloader;
mod handle;
#[cfg(test)]
mod tests;

pub use cmd::{FetchCmd, FetchResult, OnCompleteFn, OnConnectFn, Priority, ThrottleFn, WriterFn};
pub use config::DownloaderConfig;
pub use downloader::Downloader;
pub use handle::TrackHandle;
