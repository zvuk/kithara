use kithara_net::RangeSpec;
use kithara_stream::dl::FetchCmd;
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
