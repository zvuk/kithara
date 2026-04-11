//! Download command types.

use kithara_net::{Headers, RangeSpec};
use url::Url;

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

/// A single download command.
///
/// Built by protocol code and executed via
/// [`PeerHandle::execute`](super::PeerHandle::execute). The downloader
/// establishes the HTTP connection and returns a
/// [`FetchResponse`](super::FetchResponse) with headers and a body stream.
pub struct FetchCmd {
    /// HTTP method (default: [`FetchMethod::Stream`]).
    pub method: FetchMethod,
    /// URL to fetch.
    pub url: Url,
    /// Optional byte range (HTTP Range request).
    pub range: Option<RangeSpec>,
    /// Additional HTTP headers for this request.
    pub headers: Option<Headers>,
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
