//! Unified download orchestrator.
//!
//! [`Downloader`] owns the sole [`HttpClient`](kithara_net::HttpClient) and
//! routes fetch commands from registered peers. Protocols register via
//! [`Downloader::register`] and issue fetches through [`PeerHandle::execute`].

mod batch;
mod cmd;
mod config;
mod downloader;
mod peer;
mod registry;
mod response;
#[cfg(test)]
mod tests;

pub use cmd::{FetchCmd, FetchMethod, OnCompleteFn, Priority, WriterFn, reject_html_response};
pub use config::DownloaderConfig;
pub use downloader::Downloader;
pub use peer::{Peer, PeerHandle};
pub use response::{BodyStream, FetchResponse};
