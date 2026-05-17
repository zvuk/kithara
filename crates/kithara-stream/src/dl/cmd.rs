use std::io;

use kithara_events::RequestMethod;
use kithara_net::{Headers, NetError, NetResult, RangeSpec};
use tokio_util::sync::CancellationToken;
use url::Url;

/// Per-command body writer. Downloader calls it for each chunk.
pub type WriterFn = Box<dyn FnMut(&[u8]) -> io::Result<()> + Send>;

/// Per-command response callback. Fires once on the streaming path
/// when the HTTP response is in hand — past validation, headers
/// available, body about to stream. Mirrors [`OnCompleteFn`] on the
/// other end of the fetch lifecycle: peers use it to seed metadata
/// (Content-Length, Content-Type) eagerly so a reader blocked on the
/// first byte already sees a populated coord.
pub type OnResponseFn = Box<dyn FnOnce(&Headers) + Send>;

/// Per-command completion handler. Called when the fetch completes.
///
/// Receives `(bytes_written, response_headers, error)`. Headers are
/// `Some` once the HTTP response made it past validation (so `Content-Type`,
/// `Content-Length`, etc. can be captured); `None` when the fetch failed
/// before headers were received.
pub type OnCompleteFn = Box<dyn FnOnce(u64, Option<&Headers>, Option<&NetError>) + Send>;

/// Optional response-header validator for a single `FetchCmd`.
///
/// Invoked with the response headers after a successful HTTP response.
/// Returning `Err` rejects the response before the body is consumed.
pub(super) type ResponseValidator = fn(&Headers) -> NetResult<()>;

/// A single download command.
///
/// Built by protocol code via [`FetchCmd::get`] / [`FetchCmd::head`]
/// constructors and executed via
/// [`PeerHandle::execute`](super::PeerHandle::execute). The downloader
/// establishes the HTTP connection and returns a
/// [`FetchResponse`](super::FetchResponse) with headers and a body stream.
#[non_exhaustive]
pub struct FetchCmd {
    /// HTTP method.
    pub method: RequestMethod,
    /// URL to fetch.
    pub url: Url,
    /// Epoch cancel token from the Peer. When set, the Downloader
    /// combines it with the track-level cancel via [`CancelGroup`].
    pub cancel: Option<CancellationToken>,
    /// Additional HTTP headers for this request.
    pub headers: Option<Headers>,
    /// Streaming path completion handler. `None` for channel path (`execute`/`batch`).
    pub on_complete: Option<OnCompleteFn>,
    /// Streaming path response callback — fires once when the
    /// response is ready, before the body streams. `None` for the
    /// channel path (`execute`/`batch`).
    pub on_response: Option<OnResponseFn>,
    /// Optional byte range (HTTP Range request).
    pub range: Option<RangeSpec>,
    /// Optional per-request response validator.
    /// Called with the response headers after a successful HTTP response.
    /// Return `Err` to reject the response before the body is consumed.
    pub validator: Option<ResponseValidator>,
    /// Streaming path body writer. `None` for channel path (`execute`/`batch`).
    pub writer: Option<WriterFn>,
}

impl FetchCmd {
    /// HTTP GET command for the given URL.
    #[must_use]
    pub fn get(url: Url) -> Self {
        Self::with_method(RequestMethod::Get, url)
    }

    /// HTTP HEAD command for the given URL.
    #[must_use]
    pub fn head(url: Url) -> Self {
        Self::with_method(RequestMethod::Head, url)
    }

    fn with_method(method: RequestMethod, url: Url) -> Self {
        Self {
            method,
            url,
            range: None,
            headers: None,
            cancel: None,
            writer: None,
            on_complete: None,
            on_response: None,
            validator: None,
        }
    }

    /// Attach an epoch cancel token.
    #[must_use]
    pub fn cancel(mut self, cancel: Option<CancellationToken>) -> Self {
        self.cancel = cancel;
        self
    }

    /// Set additional HTTP headers.
    #[must_use]
    pub fn headers(mut self, headers: Option<Headers>) -> Self {
        self.headers = headers;
        self
    }

    /// Set byte range (HTTP Range request).
    #[must_use]
    pub fn range(mut self, range: Option<RangeSpec>) -> Self {
        self.range = range;
        self
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

    /// Set the per-command response callback (streaming path).
    #[must_use]
    pub fn on_response(mut self, cb: OnResponseFn) -> Self {
        self.on_response = Some(cb);
        self
    }

    /// Set the per-request response validator.
    #[must_use]
    pub fn validator(mut self, f: fn(&Headers) -> NetResult<()>) -> Self {
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
/// body. Pass as the validator argument to [`FetchCmd::validator`].
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
