//! Download command types.

use std::io;

use kithara_net::{Headers, RangeSpec};
use url::Url;

/// HTTP method and delivery mode for a [`FetchCmd`].
///
/// Determines how the [`Downloader`](super::Downloader) delivers the response:
///
/// | Method   | `on_connect` | `writer` | `body` in result        |
/// |----------|:------------:|:--------:|:-----------------------:|
/// | `Stream` | headers      | per-chunk| `None`                  |
/// | `Get`    | headers      | per-chunk| `Some(pool_buf)`        |
/// | `Head`   | headers      | --       | `None`                  |
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FetchMethod {
    /// HTTP GET, streaming delivery.
    ///
    /// Writer called per-chunk as bytes arrive. Body not accumulated.
    /// Use for large downloads (segments, files) that write directly to storage.
    #[default]
    Stream,
    /// HTTP GET, atomic delivery.
    ///
    /// Writer called per-chunk AND Downloader accumulates the full body into
    /// a pool buffer (zero-alloc). `on_complete` receives `body: Some(buf)`.
    /// Use for small control-plane requests (playlists, DRM keys).
    Get,
    /// HTTP HEAD — headers only, no body.
    ///
    /// Writer is not called. Use for metadata queries (Content-Length).
    Head,
}

/// Callback invoked when the HTTP connection is established.
pub type OnConnectFn = Box<dyn FnOnce(&Headers) + Send>;

/// Streaming writer — writes each chunk to storage as bytes arrive.
pub type WriterFn = Box<dyn FnMut(&[u8]) -> io::Result<()> + Send>;

/// Completion observer — called with a reference to the result.
///
/// Receives `&FetchResult` (not owned) so the caller of
/// [`Downloader::execute`](super::Downloader::execute) can both observe
/// the result via callback AND receive it as the return value.
pub type OnCompleteFn = Box<dyn FnOnce(&FetchResult) + Send>;

/// Backpressure check — returns `true` when download is too far ahead of reader.
pub type ThrottleFn = Box<dyn Fn() -> bool + Send + Sync>;

/// Result delivered after a fetch completes.
pub enum FetchResult {
    /// Fetch succeeded.
    Ok {
        /// Total bytes written via the writer callback.
        bytes_written: u64,
        /// HTTP response headers (Content-Length, Content-Type, etc.).
        headers: Headers,
        /// Full response body (only for [`FetchMethod::Get`]).
        ///
        /// Accumulated by the Downloader from a pool buffer (zero-alloc).
        /// `None` for `Stream` and `Head` methods.
        body: Option<Vec<u8>>,
    },
    /// Fetch failed.
    Err(kithara_net::NetError),
}

/// A single download command yielded by a protocol stream.
///
/// Protocols yield these through their `Stream<Item = FetchCmd>` implementation.
/// The [`Downloader`](super::Downloader) polls protocol streams and executes
/// fetches using its sole [`HttpClient`](kithara_net::HttpClient).
pub struct FetchCmd {
    /// HTTP method and delivery mode (default: [`FetchMethod::Stream`]).
    pub method: FetchMethod,
    /// URL to fetch.
    pub url: Url,
    /// Optional byte range (HTTP Range request).
    pub range: Option<RangeSpec>,
    /// Additional HTTP headers for this request.
    pub headers: Option<Headers>,
    /// Called once when the HTTP connection is established, before any body
    /// chunks. Receives response headers (Content-Length, Content-Type, etc.).
    pub on_connect: Option<OnConnectFn>,
    /// Called with each chunk of bytes as they arrive from the network.
    /// `None` for `Head` or when no per-chunk processing is needed.
    pub writer: Option<WriterFn>,
    /// Called once when the fetch completes (success or failure).
    /// Receives `&FetchResult` — observes but does not own the result.
    pub on_complete: Option<OnCompleteFn>,
    /// Backpressure: if set, Downloader pauses between chunks while this
    /// returns `true`. Checked after each writer call.
    pub throttle: Option<ThrottleFn>,
}

impl std::fmt::Debug for FetchCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchCmd")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("range", &self.range)
            .field("on_connect", &self.on_connect.as_ref().map(|_| ".."))
            .finish_non_exhaustive()
    }
}
