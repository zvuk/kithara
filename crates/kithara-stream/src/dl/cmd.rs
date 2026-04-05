//! Download command types.

use std::io;

use kithara_net::{Headers, RangeSpec};
use url::Url;

/// Priority for download commands.
///
/// Downloader sorts ready commands by priority (High first) before executing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Background prefetch.
    Low = 0,
    /// Normal sequential download.
    Normal = 1,
    /// Seek/demand — highest priority.
    High = 2,
}

/// Callback invoked when the HTTP connection is established.
pub type OnConnectFn = Box<dyn FnOnce(&Headers) + Send>;

/// Streaming writer — writes each chunk to `AssetStore` as bytes arrive.
pub type WriterFn = Box<dyn FnMut(&[u8]) -> io::Result<()> + Send>;

/// Completion callback — commits the resource and updates FSM state.
pub type OnCompleteFn = Box<dyn FnOnce(FetchResult) + Send>;

/// Backpressure check — returns `true` when download is too far ahead of reader.
pub type ThrottleFn = Box<dyn Fn() -> bool + Send + Sync>;

/// Result delivered to the `on_complete` callback.
pub enum FetchResult {
    /// Fetch succeeded.
    Ok {
        /// Total bytes written via the writer callback.
        bytes_written: u64,
        /// HTTP response headers (Content-Length, Content-Type, etc.).
        headers: Headers,
    },
    /// Fetch failed.
    Err(kithara_net::NetError),
}

/// A single download command yielded by a protocol stream.
///
/// Protocols yield these through their `Stream<Item = FetchCmd>` implementation.
/// The [`Downloader`](super::Downloader) polls protocol streams, collects ready
/// commands, sorts by [`priority`](Self::priority), and executes fetches using
/// its sole [`HttpClient`](kithara_net::HttpClient).
pub struct FetchCmd {
    /// URL to fetch.
    pub url: Url,
    /// Optional byte range (HTTP Range request).
    pub range: Option<RangeSpec>,
    /// Additional HTTP headers for this request.
    pub headers: Option<Headers>,
    /// Download priority for batch ordering.
    pub priority: Priority,
    /// Called once when the HTTP connection is established, before any body
    /// chunks. Receives response headers (Content-Length, Content-Type, etc.).
    pub on_connect: Option<OnConnectFn>,
    /// Called with each chunk of bytes as they arrive from the network.
    pub writer: WriterFn,
    /// Called once when the fetch completes (success or failure).
    pub on_complete: OnCompleteFn,
    /// Backpressure: if set, Downloader pauses between chunks while this
    /// returns `true`. Checked after each writer call.
    pub throttle: Option<ThrottleFn>,
}

impl std::fmt::Debug for FetchCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchCmd")
            .field("url", &self.url)
            .field("range", &self.range)
            .field("priority", &self.priority)
            .field("on_connect", &self.on_connect.as_ref().map(|_| ".."))
            .finish_non_exhaustive()
    }
}
