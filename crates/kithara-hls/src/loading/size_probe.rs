#![forbid(unsafe_code)]

//! HEAD-probe helper for variant size maps.
//!
//! Owns the [`PeerHandle`] + base headers needed to issue
//! `Content-Length` HEAD probes for HLS segments. Used by
//! [`crate::scheduler::HlsScheduler::calculate_size_map`].

use kithara_net::Headers;
use kithara_stream::dl::{FetchCmd, FetchMethod, PeerHandle};
use url::Url;

use crate::{HlsError, HlsResult};

/// Issues HEAD requests via the unified [`PeerHandle`] to read the
/// `Content-Length` header for a URL. Stateless apart from the
/// downloader handle and the base headers it forwards on every probe.
#[derive(Clone)]
pub(crate) struct SizeMapProbe {
    downloader: PeerHandle,
    headers: Option<Headers>,
}

impl SizeMapProbe {
    pub(crate) fn new(downloader: PeerHandle, headers: Option<Headers>) -> Self {
        Self {
            downloader,
            headers,
        }
    }

    /// Read the `Content-Length` header for `url` via a HEAD request.
    ///
    /// # Errors
    /// Returns an error when the network request fails, the
    /// `Content-Length` header is missing, or its value cannot be
    /// parsed as a `u64`.
    pub(crate) async fn get_content_length(&self, url: &Url) -> HlsResult<u64> {
        let cmd = FetchCmd {
            method: FetchMethod::Head,
            url: url.clone(),
            range: None,
            headers: self.headers.clone(),
        };
        let resp = self.downloader.execute(cmd).await.map_err(HlsError::from)?;
        let resp_headers = resp.headers;
        let content_length = resp_headers
            .get("content-length")
            .or_else(|| resp_headers.get("Content-Length"))
            .ok_or_else(|| {
                HlsError::InvalidUrl(format!(
                    "No Content-Length header in HEAD response for {url}",
                ))
            })?;

        content_length.parse::<u64>().map_err(|e| {
            HlsError::InvalidUrl(format!(
                "Invalid Content-Length '{content_length}' for {url}: {e}",
            ))
        })
    }
}
