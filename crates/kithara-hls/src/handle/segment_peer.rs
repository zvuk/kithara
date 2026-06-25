use kithara_net::{Headers, RangeSpec};
use kithara_platform::CancelToken;
use kithara_stream::dl::{FetchCmd, OnCompleteFn, WriterFn};
use url::Url;

use crate::config::SizeProbeMethod;

/// Narrow per-segment transport surface for lazy size probes.
pub(crate) struct SegmentPeer {
    headers: Option<Headers>,
}

impl SegmentPeer {
    pub(crate) fn new(headers: Option<Headers>) -> Self {
        Self { headers }
    }

    pub(crate) fn size_probe(
        &self,
        url: Url,
        method: SizeProbeMethod,
        cancel: CancelToken,
        writer: WriterFn,
        on_complete: OnCompleteFn,
    ) -> FetchCmd {
        match method {
            SizeProbeMethod::Head => FetchCmd::head(url)
                .cancel(cancel)
                .maybe_headers(self.headers.clone())
                .writer(writer)
                .on_complete(on_complete)
                .build(),
            SizeProbeMethod::RangeGet => FetchCmd::get(url)
                .range(RangeSpec::new(0, Some(0)))
                .cancel(cancel)
                .maybe_headers(self.headers.clone())
                .writer(writer)
                .on_complete(on_complete)
                .build(),
        }
    }
}

/// Resolve a byte length from a probe response: the `Content-Range` total
/// (`bytes a-b/total`) when present, else `Content-Length`, else `0`.
pub(crate) fn parse_size_headers(headers: &Headers) -> u64 {
    if let Some(total) = headers
        .get("content-range")
        .or_else(|| headers.get("Content-Range"))
        .and_then(|h| h.split('/').nth(1))
        .filter(|s| *s != "*")
        .and_then(|s| s.parse::<u64>().ok())
    {
        return total;
    }
    headers
        .get("content-length")
        .or_else(|| headers.get("Content-Length"))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}
