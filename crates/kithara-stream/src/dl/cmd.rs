//! Download command types.

use std::io;

use derive_setters::Setters;
use kithara_net::{Headers, NetError, RangeSpec};
use tokio_util::sync::CancellationToken;
use url::Url;

/// Scheduling priority for download commands and peers.
///
/// Higher-priority commands are processed first. `High` commands
/// bypass throttle checks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Latency-sensitive: demand segments, imperative `execute()` calls.
    High = 0,
    /// Background: batch segment downloads, pre-fetching.
    #[default]
    Normal = 1,
    /// Throttled: peer is ahead of the reader. Downloader defers execution
    /// until `peer.priority()` rises above `Low`.
    Low = 2,
}

/// HTTP method for a [`FetchCmd`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FetchMethod {
    /// HTTP GET, streaming delivery.
    ///
    /// Body delivered as an async [`BodyStream`](super::BodyStream).
    /// Use for large downloads (segments, files) that write directly to storage.
    #[default]
    Stream,
    /// HTTP HEAD — headers only, no body.
    ///
    /// Use for metadata queries (Content-Length).
    Head,
}

/// Per-command body writer. Downloader calls it for each chunk.
pub type WriterFn = Box<dyn FnMut(&[u8]) -> io::Result<()> + Send>;

/// Per-command completion handler. Called when the fetch completes.
pub type OnCompleteFn = Box<dyn FnOnce(u64, Option<&NetError>) + Send>;

/// A single download command.
///
/// Built by protocol code via [`FetchCmd::get`] / [`FetchCmd::head`]
/// constructors and executed via
/// [`PeerHandle::execute`](super::PeerHandle::execute). The downloader
/// establishes the HTTP connection and returns a
/// [`FetchResponse`](super::FetchResponse) with headers and a body stream.
#[non_exhaustive]
#[derive(Setters)]
pub struct FetchCmd {
    /// HTTP method (default: [`FetchMethod::Stream`]).
    #[setters(skip)]
    pub method: FetchMethod,
    /// URL to fetch.
    #[setters(skip)]
    pub url: Url,
    /// Optional byte range (HTTP Range request).
    pub range: Option<RangeSpec>,
    /// Additional HTTP headers for this request.
    pub headers: Option<Headers>,
    /// Epoch cancel token from the Peer. When set, the Downloader
    /// combines it with the track-level cancel via [`CancelGroup`].
    pub cancel: Option<CancellationToken>,
    /// Streaming path body writer. `None` for channel path (`execute`/`batch`).
    #[setters(skip)]
    pub writer: Option<WriterFn>,
    /// Streaming path completion handler. `None` for channel path (`execute`/`batch`).
    #[setters(skip)]
    pub on_complete: Option<OnCompleteFn>,
}

impl FetchCmd {
    /// HTTP GET command for the given URL.
    #[must_use]
    pub fn get(url: Url) -> Self {
        Self {
            method: FetchMethod::Stream,
            url,
            range: None,
            headers: None,
            cancel: None,
            writer: None,
            on_complete: None,
        }
    }

    /// HTTP HEAD command for the given URL.
    #[must_use]
    pub fn head(url: Url) -> Self {
        Self {
            method: FetchMethod::Head,
            url,
            range: None,
            headers: None,
            cancel: None,
            writer: None,
            on_complete: None,
        }
    }

    /// Set the per-command body writer (streaming path).
    #[must_use]
    pub fn writer(mut self, w: WriterFn) -> Self {
        self.writer = Some(w);
        self
    }

    /// Set the per-command completion handler (streaming path).
    #[must_use]
    pub fn on_complete(mut self, cb: OnCompleteFn) -> Self {
        self.on_complete = Some(cb);
        self
    }
}

impl std::fmt::Debug for FetchCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchCmd")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}
