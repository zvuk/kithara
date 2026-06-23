use kithara_net::NetError;
use kithara_stream::dl::{FetchCmd, FetchResponse, PeerHandle};

use crate::handle::SegmentPeer;

/// Variant-scoped fetch surface over a shared [`PeerHandle`]. Owns the
/// drain-chunk concurrency loop for size-probe batches and mints
/// [`SegmentPeer`]s carrying the variant's request headers.
pub(crate) struct VariantPeer {
    peer: PeerHandle,
    headers: Option<kithara_net::Headers>,
}

impl VariantPeer {
    pub(crate) fn new(peer: PeerHandle, headers: Option<kithara_net::Headers>) -> Self {
        Self { peer, headers }
    }

    /// Run `cmds` through the peer in chunks of at most `concurrency` in
    /// flight, preserving array order. `concurrency` is normalised to `1`
    /// by the caller. Returns an empty `Vec` for empty `cmds`.
    pub(crate) async fn probe_batch(
        &self,
        cmds: Vec<FetchCmd>,
        concurrency: usize,
    ) -> Vec<Result<FetchResponse, NetError>> {
        let mut results: Vec<_> = Vec::with_capacity(cmds.len());
        let mut remaining = cmds;
        while !remaining.is_empty() {
            let take = concurrency.min(remaining.len());
            let chunk: Vec<FetchCmd> = remaining.drain(..take).collect();
            results.extend(self.peer.batch(chunk).await);
        }
        results
    }

    /// A [`SegmentPeer`] carrying this variant's request headers.
    pub(crate) fn segment_peer(&self) -> SegmentPeer {
        SegmentPeer::new(self.headers.clone())
    }
}
