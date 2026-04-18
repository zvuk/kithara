//! Download command types.

use std::io;

use derive_setters::Setters;
use kithara_net::{Headers, NetError, NetResult, RangeSpec};
use tokio_util::sync::CancellationToken;
use url::Url;

/// Scheduling priority for download commands and peers.
///
/// Used in a 2×2 slot map: (peer priority) × (cmd priority).
/// `High` commands and peers are processed before `Low`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Latency-sensitive: demand segments, `execute`/`batch` calls, seek.
    High = 0,
    /// Background: prefetch, idle downloads.
    #[default]
    Low = 1,
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
    /// Optional per-request response validator.
    /// Called with the response headers after a successful HTTP response.
    /// Return `Err` to reject the response before the body is consumed.
    #[setters(skip)]
    pub validator: Option<fn(&Headers) -> NetResult<()>>,
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
            validator: None,
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
            validator: None,
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

    /// Set the per-request response validator.
    #[must_use]
    pub fn with_validator(mut self, f: fn(&Headers) -> NetResult<()>) -> Self {
        self.validator = Some(f);
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

/// Reject responses with `content-type: text/html`.
///
/// Protects against CDN soft-error pages that return `200 OK` with an HTML
/// body. Pass as the validator argument to [`FetchCmd::with_validator`].
///
/// # Errors
///
/// Returns [`NetError::InvalidContentType`] when the response `content-type`
/// header starts with `text/html`.
pub fn reject_html_response(headers: &Headers) -> NetResult<()> {
    if let Some(ct) = headers.get("content-type")
        && ct.starts_with("text/html")
    {
        return Err(NetError::InvalidContentType(ct.to_string()));
    }
    Ok(())
}
