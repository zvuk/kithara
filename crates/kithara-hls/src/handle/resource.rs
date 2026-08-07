use std::{io::ErrorKind, ops::Range};

use kithara_assets::{
    AssetScope, AssetsError, AssetsResult, ReadSide, ResourceAcquisition, ResourceKey,
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

    /// Committed length per the availability manifest — the skip-fetch
    /// guard's size source. The manifest is the sole byte authority: a file
    /// it does not vouch for reads as absent here, the guard dispatches the
    /// fetch, and the acquire path deletes the torn leftover and refetches.
    pub(crate) fn committed_len(&self) -> Option<u64> {
        self.scope.store().final_len(self.key)
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

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::fs;

    use kithara_assets::{
        AcquisitionResult, AssetResource, AssetSource, AssetStore, StorageBackend, WriteSide,
    };
    use kithara_test_utils::kithara;

    use super::*;

    fn segment_fixture(dir: &std::path::Path) -> (AssetScope, ResourceKey, Url) {
        let store = AssetStore::builder()
            .backend(StorageBackend::Disk { root: dir.into() })
            .build();
        let url = Url::parse("https://example.com/media/seg0.m4s").expect("segment url");
        let scope = store
            .scope::<crate::Hls>(&AssetSource::Remote {
                url: Url::parse("https://example.com/master.m3u8").expect("master url"),
                discriminator: None,
            })
            .expect("scope");
        let key = scope.key(&AssetResource::Url(url.clone())).expect("key");
        (scope, key, url)
    }

    /// The skip-fetch guard and the readiness gate must consult the same
    /// byte authority — the availability manifest. A segment file the
    /// manifest does not vouch for is a torn write: the guard reports no
    /// committed length, the fetch is dispatched, and the acquire path
    /// replaces the leftover. A metadata-based answer here marks the slot
    /// `Loaded` while the readiness gate keeps answering "absent", and
    /// playback wedges on a warm cache with no manifest.
    #[kithara::test]
    fn committed_len_answers_from_the_availability_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (scope, key, url) = segment_fixture(dir.path());

        let path = dir
            .path()
            .join(scope.asset_root())
            .join(key.rel_path().expect("relative key"));
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, b"unvouched-bytes").expect("write");

        let handle = ResourceHandle::new(&scope, &key, &url);
        assert_eq!(
            handle.committed_len(),
            None,
            "a file the manifest does not vouch for must be refetched, never sized"
        );

        let AcquisitionResult::Pending(writer) =
            scope.store().acquire_resource(&key, None).expect("acquire")
        else {
            panic!("unvouched file must reacquire as Pending");
        };
        writer.write_at(0, b"committed-bytes").expect("write_at");
        drop(writer.commit(Some(15)).expect("commit"));

        assert_eq!(
            handle.committed_len(),
            Some(15),
            "a commit through the store vouches the bytes and sizes the guard"
        );
    }
}
