use kithara_net::RangeSpec;
use kithara_stream::dl::{FetchCmd, FetchResponse};
use url::Url;

/// Narrow per-segment fetch surface: builds size-probe [`FetchCmd`]s for a
/// single resource URL, carrying the shared request headers forward. The two
/// builders mirror the two `SizeProbeMethod` arms — the caller selects which
/// to call.
pub(crate) struct SegmentPeer {
    headers: Option<kithara_net::Headers>,
}

impl SegmentPeer {
    pub(crate) fn new(headers: Option<kithara_net::Headers>) -> Self {
        Self { headers }
    }

    /// `HEAD` probe — size comes from the `Content-Length` response header.
    pub(crate) fn head(&self, url: Url) -> FetchCmd {
        FetchCmd::head(url)
            .maybe_headers(self.headers.clone())
            .build()
    }

    /// Single-byte ranged `GET` probe — size comes from the `Content-Range`
    /// total. Used against upstreams that reject or mishandle `HEAD`.
    pub(crate) fn range_probe(&self, url: Url) -> FetchCmd {
        FetchCmd::get(url)
            .range(RangeSpec::new(0, Some(0)))
            .maybe_headers(self.headers.clone())
            .build()
    }
}

/// Resolve a byte length from a probe response: the `Content-Range` total
/// (`bytes a-b/total`) when present, else `Content-Length`, else `0`.
pub(crate) fn parse_size(resp: &FetchResponse) -> u64 {
    if let Some(total) = resp
        .headers
        .get("content-range")
        .or_else(|| resp.headers.get("Content-Range"))
        .and_then(|h| h.split('/').nth(1))
        .filter(|s| *s != "*")
        .and_then(|s| s.parse::<u64>().ok())
    {
        return total;
    }
    resp.headers
        .get("content-length")
        .or_else(|| resp.headers.get("Content-Length"))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}
