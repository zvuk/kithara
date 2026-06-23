use std::{io::ErrorKind, ops::Range};

use kithara_assets::{
    AssetResource, AssetResourceState, AssetScope, AssetsError, AssetsResult, ReadSide, ResourceKey,
};
use kithara_drm::DecryptContext;
use kithara_stream::{StreamError, StreamResult};
use url::Url;

use crate::{HlsError, segment::SegmentContent};

/// Narrow per-segment view over the variant's on-disk resource. A segment (or
/// its init prefix) talks to disk only through this surface — `read_at` /
/// `contains` / `committed_len` for reads, `acquire` for the write path —
/// instead of reaching into [`AssetScope`] / its [`store`](AssetScope::store)
/// directly. There is deliberately no `store()` accessor: the handle exposes
/// only what a segment needs (narrow-handle invariant).
///
/// Construction is cheap — it clones the shared (Arc-backed) [`AssetScope`]
/// plus the segment's [`ResourceKey`] and [`Url`] — so each segment/entry can
/// own one. At this stage it is a thin façade: every method routes to the same
/// `scope` / `scope.store().open_resource` op the call site ran before. The
/// held-resource lease optimization is deferred.
pub(crate) struct ResourceHandle {
    scope: AssetScope<DecryptContext>,
    key: ResourceKey,
    url: Url,
}

impl ResourceHandle {
    pub(crate) fn new(scope: AssetScope<DecryptContext>, key: ResourceKey, url: Url) -> Self {
        Self { scope, key, url }
    }

    /// The segment URL this handle fetches from — consumed by the fetch path
    /// when building the `FetchCmd` after [`acquire`](Self::acquire).
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    /// Open the resource and copy `range` into `dst`. `Ok(None)` means the
    /// resource is not on disk yet (`NotFound`) — the caller treats that as a
    /// pending read.
    pub(crate) fn read_at(&self, range: Range<u64>, dst: &mut [u8]) -> StreamResult<Option<usize>> {
        let resource = match self.scope.store().open_resource(&self.key, None) {
            Ok(res) => res,
            Err(AssetsError::Io(e)) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StreamError::Source(HlsError::from(e).into())),
        };
        resource
            .wait_range(range.clone())
            .map_err(|e| StreamError::Source(HlsError::from(e).into()))?;
        let n = resource
            .read_at(range.start, dst)
            .map_err(|e| StreamError::Source(HlsError::from(e).into()))?;
        Ok(Some(n))
    }

    /// Whether every byte in `range` is already present on disk for this
    /// resource (or `range` is empty).
    pub(crate) fn contains(&self, range: Range<u64>) -> bool {
        self.scope.store().contains_range(&self.key, range)
    }

    /// Committed on-disk length when the resource is `Committed` with a known
    /// `final_len` — the skip-fetch guard's size source.
    pub(crate) fn committed_len(&self) -> Option<u64> {
        match self.scope.store().resource_state(&self.key) {
            Ok(AssetResourceState::Committed { final_len }) => final_len,
            _ => None,
        }
    }

    /// Acquire the resource for the write path, branching on the segment's
    /// decryption disposition: `Plain` acquires cleartext; `Encrypted` carries
    /// the AES-128 [`DecryptContext`] forward as the processing context.
    pub(crate) fn acquire(
        &self,
        content: &SegmentContent,
    ) -> AssetsResult<AssetResource<DecryptContext>> {
        match content {
            SegmentContent::Plain => self.scope.store().acquire_resource(&self.key, None),
            SegmentContent::Encrypted(c) => {
                self.scope
                    .store()
                    .acquire_resource_with_ctx(&self.key, None, Some(c.clone()))
            }
        }
    }
}
