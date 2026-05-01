//! Download command types.

use std::io;

use derive_setters::Setters;
use kithara_events::RequestMethod;
use kithara_net::{Headers, NetError, NetResult, RangeSpec};
use tokio_util::sync::CancellationToken;
use url::Url;

// `RequestMethod` and `RequestPriority` are defined in `kithara-events`
// (next to the lifecycle events that quote them). Re-exported below
// under the same names so `kithara_stream::dl::*` callers keep working.

/// Per-command body writer. Downloader calls it for each chunk.
pub type WriterFn = Box<dyn FnMut(&[u8]) -> io::Result<()> + Send>;

/// Per-command completion handler. Called when the fetch completes.
pub type OnCompleteFn = Box<dyn FnOnce(u64, Option<&NetError>) + Send>;

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
#[derive(Setters)]
pub struct FetchCmd {
    /// Epoch cancel token from the Peer. When set, the Downloader
    /// combines it with the track-level cancel via [`CancelGroup`].
    pub cancel: Option<CancellationToken>,
    /// Additional HTTP headers for this request.
    pub headers: Option<Headers>,
    /// Streaming path completion handler. `None` for channel path (`execute`/`batch`).
    #[setters(skip)]
    pub on_complete: Option<OnCompleteFn>,
    /// Optional byte range (HTTP Range request).
    pub range: Option<RangeSpec>,
    /// Optional per-request response validator.
    /// Called with the response headers after a successful HTTP response.
    /// Return `Err` to reject the response before the body is consumed.
    #[setters(skip)]
    pub validator: Option<ResponseValidator>,
    /// Streaming path body writer. `None` for channel path (`execute`/`batch`).
    #[setters(skip)]
    pub writer: Option<WriterFn>,
    /// HTTP method (default: [`RequestMethod::Get`]).
    #[setters(skip)]
    pub method: RequestMethod,
    /// URL to fetch.
    #[setters(skip)]
    pub url: Url,
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

    /// Set the per-command completion handler (streaming path).
    #[must_use]
    pub fn on_complete(mut self, cb: OnCompleteFn) -> Self {
        self.on_complete = Some(cb);
        self
    }

    /// Build a [`FetchCmd`] with the given method and URL; all other
    /// fields start unset.
    fn with_method(method: RequestMethod, url: Url) -> Self {
        Self {
            method,
            url,
            range: None,
            headers: None,
            cancel: None,
            writer: None,
            on_complete: None,
            validator: None,
        }
    }

    /// Set the per-request response validator.
    #[must_use]
    pub fn with_validator(mut self, f: fn(&Headers) -> NetResult<()>) -> Self {
        self.validator = Some(f);
        self
    }

    /// Set the per-command body writer (streaming path).
    #[must_use]
    pub fn writer(mut self, w: WriterFn) -> Self {
        self.writer = Some(w);
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
