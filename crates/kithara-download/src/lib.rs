//! Unified download orchestrator.
//!
//! [`Downloader`] owns the sole [`HttpClient`](kithara_net::HttpClient) and
//! routes fetch commands from registered peers. Protocols register via
//! [`Downloader::register`] and issue fetches through [`PeerHandle::execute`].

#![forbid(unsafe_code)]

mod batch;
mod cmd;
mod config;
mod downloader;
mod event;
mod peer;
mod registry;
mod request;
mod response;
#[cfg(test)]
mod tests;

pub use cmd::{
    DemandFn, FetchCmd, OnCompleteFn, OnResponseFn, OnSlowFn, WriterFn, reject_html_response,
};
pub use config::{DownloaderConfig, DownloaderConfigPatch};
pub use downloader::Downloader;
pub use event::{CancelReason, DownloaderEvent, RequestId, RequestMethod, RequestPriority};
use humantime_serde as _;
pub use peer::{Peer, PeerHandle};
pub use response::{BodyStream, FetchResponse};
mod consts;
