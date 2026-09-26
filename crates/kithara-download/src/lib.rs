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
mod response;
/// This module tests the HTTP download layer, and Miri can reach neither half of
/// it: the shared client initialises `aws-lc` — a C library Miri cannot enter —
/// and the tests that reach a server bind a real socket on 127.0.0.1, which
/// `fcntl(F_SETFD)` refuses under Miri. A transport double would test the double.
#[cfg(all(test, not(miri)))]
#[path = "../tests/download.rs"]
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
