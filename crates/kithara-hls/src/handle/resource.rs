use std::{io::ErrorKind, ops::Range};

use kithara_assets::{
    AssetResourceState, AssetScope, AssetsError, AssetsResult, ReadSide, ResourceAcquisition,
    ResourceKey,
};
use kithara_stream::{StreamError, StreamResult};
use url::Url;

use crate::{HlsError, decrypt_processor::as_process_ctx, segment::SegmentContent};

#[derive(Clone, Copy, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ResourceHandle<'a> {
    scope: &'a AssetScope,
    key: &'a ResourceKey,
    #[field(get, vis = "pub(crate)")]
    url: &'a Url,
}

impl<'a> ResourceHandle<'a> {
    pub(crate) fn new(scope: &'a AssetScope, key: &'a ResourceKey, url: &'a Url) -> Self {
        Self { scope, key, url }
    }

    /// Acquire the resource for the write path, branching on the segment's
    /// decryption disposition: `Plain` acquires cleartext; `Encrypted` carries
    /// the AES-128 [`DecryptContext`] forward as the processing context.
    pub(crate) fn acquire(&self, content: &SegmentContent) -> AssetsResult<ResourceAcquisition> {
        match content {
            SegmentContent::Plain => self.scope.store().acquire_resource(self.key, None),
            SegmentContent::Encrypted(c) => self.scope.store().acquire_resource_with_ctx(
                self.key,
                None,
                Some(as_process_ctx(c.clone())),
            ),
        }
    }

    /// Committed on-disk length when the resource is `Committed` with a known
    /// `final_len` — the skip-fetch guard's size source.
    pub(crate) fn committed_len(&self) -> Option<u64> {
        match self.scope.store().resource_state(self.key) {
            Ok(AssetResourceState::Committed { final_len }) => final_len,
            _ => None,
        }
    }

    /// Whether every byte in `range` is already present on disk for this
    /// resource (or `range` is empty).
    pub(crate) fn contains(&self, range: Range<u64>) -> bool {
        self.scope.store().contains_range(self.key, range)
    }

    /// Open the resource and copy `range` into `dst`. `Ok(None)` means the
    /// resource is not on disk yet (`NotFound`) — the caller treats that as a
    /// pending read.
    pub(crate) fn read_at(&self, range: Range<u64>, dst: &mut [u8]) -> StreamResult<Option<usize>> {
        let resource = match self.scope.store().open_resource(self.key, None) {
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
}
