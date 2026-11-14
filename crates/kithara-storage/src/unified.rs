#![forbid(unsafe_code)]

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use kithara_platform::{CancelToken, sync::Arc};

use crate::{
    AtomicChunked, MemDriver, MemResource, StorageResult,
    resource::{ResourceStatus, WaitOutcome},
};
#[cfg(not(target_arch = "wasm32"))]
use crate::{MmapDriver, MmapResource, ResourceRead};

/// Cloneable disk/memory resource, uniformly wrapped in [`AtomicChunked`].
/// Fresh segment writes use tmp-and-rename atomic commits; committed reopens and memory
/// use passthrough mode, so callers cannot bypass the selected commit protection.
#[derive(Clone, Debug)]
pub enum StorageResource {
    /// File-backed mmap resource (atomic or passthrough decorator).
    #[cfg(not(target_arch = "wasm32"))]
    Mmap(Arc<AtomicChunked<MmapDriver>>),
    /// In-memory resource (always passthrough — memory has no
    /// torn-write hazard).
    Mem(Arc<AtomicChunked<MemDriver>>),
}

#[cfg(not(target_arch = "wasm32"))]
impl From<MmapResource> for StorageResource {
    fn from(r: MmapResource) -> Self {
        let path = r.path().map(Path::to_path_buf).unwrap_or_default();
        Self::Mmap(Arc::new(AtomicChunked::passthrough(r, path)))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<AtomicChunked<MmapDriver>> for StorageResource {
    fn from(r: AtomicChunked<MmapDriver>) -> Self {
        Self::Mmap(Arc::new(r))
    }
}

impl From<MemResource> for StorageResource {
    fn from(r: MemResource) -> Self {
        Self::Mem(Arc::new(AtomicChunked::passthrough(r, PathBuf::default())))
    }
}

/// Shared, cheaply-cloneable resource handle delegating to the wrapped
/// [`AtomicChunked`]. The split-handle typestate lives below this layer; this
/// unified enum is the multi-owner facade used by the asset cache.
impl StorageResource {
    /// Returns `true` if the resource has been committed with zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Backing file path, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.path(),
            Self::Mem(_) => None,
        }
    }

    /// Write entire contents and commit atomically.
    ///
    /// # Errors
    /// Returns error if the write or commit fails.
    pub fn write_all(&self, data: &[u8]) -> StorageResult<()> {
        self.write_at(0, data)?;
        self.commit(Some(data.len() as u64))
    }

    delegate::delegate! {
        to match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(resource) => resource,
            Self::Mem(resource) => resource,
        } {
            /// Release the writer without failing the resource, for a caller that owns
            /// the refill. See [`AtomicChunked::abandon`].
            pub fn abandon(&self);
            /// Commit the resource.
            ///
            /// # Errors
            /// Returns error if the backend cannot finalize.
            pub fn commit(&self, final_len: Option<u64>) -> StorageResult<()>;
            /// Whether the given range is fully covered by available data.
            #[must_use]
            pub fn contains_range(&self, range: Range<u64>) -> bool;
            /// Mark the resource failed.
            pub fn fail(&self, reason: String);
            /// Committed length, if known.
            #[must_use]
            pub fn len(&self) -> Option<u64>;
            /// First gap in available data starting at `from`, up to `limit`.
            #[must_use]
            pub fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
            /// Reactivate a committed resource for continued writing.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled or the backend cannot reopen.
            pub fn reactivate(&self) -> StorageResult<()>;
            /// Read data at the given offset into `buf`; returns bytes read.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled, failed, or the read fails.
            pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            /// Read the writer's own in-flight bytes from the active working storage,
            /// bypassing the committed snapshot (decrypt-on-commit read-back).
            ///
            /// # Errors
            /// Returns error if the resource is cancelled, failed, or the read fails.
            pub fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            /// Read the entire resource into a caller buffer; returns bytes read.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled, failed, or the read fails.
            pub fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize>;
            /// Current runtime status.
            #[must_use]
            pub fn status(&self) -> ResourceStatus;
            /// Wait until the given byte range is available.
            ///
            /// # Errors
            /// Returns error if the range is invalid, the resource is cancelled, or the
            /// resource has failed.
            pub fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
            /// Wait until the given byte range is available, interrupting only this
            /// wait when `cancel` fires.
            ///
            /// # Errors
            /// Returns error if the range is invalid, either cancellation token fires,
            /// or the resource has failed.
            pub fn wait_range_with_cancel(
                &self,
                range: Range<u64>,
                cancel: &CancelToken,
            ) -> StorageResult<WaitOutcome>;
            /// Write data at the given offset.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled, failed, or the write fails.
            pub fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use kithara_platform::CancelToken;

    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    use crate::{MmapOptions, OpenMode, Resource};

    fn mem_resource() -> MemResource {
        let pools = crate::test_pools::pools();
        MemResource::new(CancelToken::never(), crate::test_pools::byte_buffer(&pools))
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn mmap_variant_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mmap: MmapResource = Resource::open(
            CancelToken::never(),
            MmapOptions::for_path(path).mode(OpenMode::Auto).build(),
        )
        .unwrap();

        let res = StorageResource::from(mmap);
        res.write_at(0, b"hello mmap").unwrap();
        res.commit(Some(10)).unwrap();

        let mut buf = [0u8; 10];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 10);
        assert_eq!(&buf, b"hello mmap");
        assert!(res.path().is_some());
    }

    #[kithara::test]
    fn mem_variant_roundtrip() {
        let mem = mem_resource();
        let res = StorageResource::from(mem);

        res.write_at(0, b"hello mem").unwrap();
        res.commit(Some(9)).unwrap();

        let mut buf = [0u8; 9];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"hello mem");
        assert!(res.path().is_none());
    }

    #[kithara::test]
    fn mem_double_commit_is_idempotent() {
        let mem = mem_resource();
        let res = StorageResource::from(mem);

        res.write_at(0, b"hello mem").unwrap();
        res.commit(Some(9)).unwrap();
        // Second commit through the shared `&self` facade must be a no-op, not
        // re-snapshot the released working buffer.
        res.commit(Some(9)).unwrap();

        let mut buf = [0u8; 9];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"hello mem");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn from_mmap_resource() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conv.bin");
        let mmap: MmapResource = Resource::open(
            CancelToken::never(),
            MmapOptions::for_path(path).mode(OpenMode::Auto).build(),
        )
        .unwrap();
        let res: StorageResource = mmap.into();
        assert!(matches!(res, StorageResource::Mmap(_)));
    }

    #[kithara::test]
    fn from_mem_resource() {
        let mem = mem_resource();
        let res: StorageResource = mem.into();
        assert!(matches!(res, StorageResource::Mem(_)));
    }

    #[kithara::test]
    fn status_delegation() {
        let mem = mem_resource();
        let res = StorageResource::from(mem);

        assert_eq!(res.status(), ResourceStatus::Active);
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));
    }

    #[kithara::test]
    fn wait_range_delegation() {
        let mem = mem_resource();
        let res = StorageResource::from(mem);

        res.write_at(0, b"data").unwrap();
        let outcome = res.wait_range(0..4).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
    }

    #[kithara::test]
    fn fail_delegation() {
        let mem = mem_resource();
        let res = StorageResource::from(mem);

        res.fail("boom".to_string());
        assert_eq!(res.status(), ResourceStatus::Failed("boom".to_string()));
    }

    #[kithara::test]
    fn reactivate_delegation() {
        let mem = mem_resource();
        let res = StorageResource::from(mem);

        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

        res.reactivate().unwrap();
        assert_eq!(res.status(), ResourceStatus::Active);
    }

    #[kithara::test]
    fn reactivate_clears_failed_for_refetch() {
        let mem = mem_resource();
        let res = StorageResource::from(mem);

        res.write_at(0, b"par").unwrap();
        res.fail("fetch cancelled before completion".to_string());
        assert!(matches!(res.status(), ResourceStatus::Failed(_)));

        res.reactivate()
            .expect("reactivate must clear a prior failure so the key can be re-fetched");
        assert_eq!(res.status(), ResourceStatus::Active);

        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));
    }
}
