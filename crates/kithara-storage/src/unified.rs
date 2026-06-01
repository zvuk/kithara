#![forbid(unsafe_code)]

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    AtomicChunked, MemDriver, MemResource, ResourceRead, StorageResult,
    resource::{ResourceStatus, WaitOutcome},
};
#[cfg(not(target_arch = "wasm32"))]
use crate::{MmapDriver, MmapResource};

/// Unified resource: disk (mmap) or memory backend.
///
/// Every variant wraps its inner in an [`AtomicChunked`] decorator.
/// Fresh segment writes use the decorator in **atomic** mode (writes
/// land at `<canonical>.tmp`, atomic-renamed on commit); re-opens of
/// already-committed files and memory-backed inners use it in
/// **passthrough** mode (no atomicity, zero overhead beyond the Arc).
/// Uniform wrapping means every code path that observes a
/// `StorageResource` sees the same atomic-on-commit guarantees, and
/// no caller can accidentally bypass the protection.
///
/// `Arc` makes the variants cheaply cloneable, matching the original
/// `Resource<D>` contract — the previous direct-`Resource` enum was
/// also Clone via internal `Arc<DriverState>`.
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
    /// Commit the resource.
    ///
    /// # Errors
    /// Returns error if the backend cannot finalize.
    pub fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.commit(final_len),
            Self::Mem(r) => r.commit(final_len),
        }
    }

    /// Whether the given range is fully covered by available data.
    #[must_use]
    pub fn contains_range(&self, range: Range<u64>) -> bool {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.contains_range(range),
            Self::Mem(r) => r.contains_range(range),
        }
    }

    /// Mark the resource failed.
    pub fn fail(&self, reason: String) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.fail(reason),
            Self::Mem(r) => r.fail(reason),
        }
    }

    /// Returns `true` if the resource has been committed with zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Committed length, if known.
    #[must_use]
    // ast-grep-ignore: idioms.match-self-conversion
    pub fn len(&self) -> Option<u64> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.len(),
            Self::Mem(r) => r.len(),
        }
    }

    /// First gap in available data starting at `from`, up to `limit`.
    #[must_use]
    pub fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.next_gap(from, limit),
            Self::Mem(r) => r.next_gap(from, limit),
        }
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

    /// Reactivate a committed resource for continued writing.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled or the backend cannot reopen.
    // ast-grep-ignore: idioms.match-self-conversion
    pub fn reactivate(&self) -> StorageResult<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.reactivate(),
            Self::Mem(r) => r.reactivate(),
        }
    }

    /// Read data at the given offset into `buf`; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.read_at(offset, buf),
            Self::Mem(r) => r.read_at(offset, buf),
        }
    }

    /// Read the writer's own in-flight bytes from the active working storage,
    /// bypassing the committed snapshot (decrypt-on-commit read-back).
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.read_inflight_at(offset, buf),
            Self::Mem(r) => r.read_inflight_at(offset, buf),
        }
    }

    /// Read the entire resource into a caller buffer; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.read_into(buf),
            Self::Mem(r) => r.read_into(buf),
        }
    }

    /// Current runtime status.
    #[must_use]
    // ast-grep-ignore: idioms.match-self-conversion
    pub fn status(&self) -> ResourceStatus {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.status(),
            Self::Mem(r) => r.status(),
        }
    }

    /// Wait until the given byte range is available.
    ///
    /// # Errors
    /// Returns error if the range is invalid, the resource is cancelled, or the
    /// resource has failed.
    pub fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.wait_range(range),
            Self::Mem(r) => r.wait_range(range),
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

    /// Write data at the given offset.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the write fails.
    pub fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.write_at(offset, data),
            Self::Mem(r) => r.write_at(offset, data),
        }
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use kithara_platform::{CancellationToken, time::Duration};

    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    use crate::{MmapOptions, OpenMode, Resource};

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn mmap_variant_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mmap: MmapResource = Resource::open(
            CancellationToken::default(),
            MmapOptions {
                path,
                initial_len: None,
                mode: OpenMode::Auto,
            },
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

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn mem_variant_roundtrip() {
        let mem = MemResource::new(CancellationToken::default());
        let res = StorageResource::from(mem);

        res.write_at(0, b"hello mem").unwrap();
        res.commit(Some(9)).unwrap();

        let mut buf = [0u8; 9];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"hello mem");
        assert!(res.path().is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn from_mmap_resource() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conv.bin");
        let mmap: MmapResource = Resource::open(
            CancellationToken::default(),
            MmapOptions {
                path,
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .unwrap();
        let res: StorageResource = mmap.into();
        assert!(matches!(res, StorageResource::Mmap(_)));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn from_mem_resource() {
        let mem = MemResource::new(CancellationToken::default());
        let res: StorageResource = mem.into();
        assert!(matches!(res, StorageResource::Mem(_)));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn status_delegation() {
        let mem = MemResource::new(CancellationToken::default());
        let res = StorageResource::from(mem);

        assert_eq!(res.status(), ResourceStatus::Active);
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn wait_range_delegation() {
        let mem = MemResource::new(CancellationToken::default());
        let res = StorageResource::from(mem);

        res.write_at(0, b"data").unwrap();
        let outcome = res.wait_range(0..4).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn fail_delegation() {
        let mem = MemResource::new(CancellationToken::default());
        let res = StorageResource::from(mem);

        res.fail("boom".to_string());
        assert_eq!(res.status(), ResourceStatus::Failed("boom".to_string()));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn reactivate_delegation() {
        let mem = MemResource::new(CancellationToken::default());
        let res = StorageResource::from(mem);

        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

        res.reactivate().unwrap();
        assert_eq!(res.status(), ResourceStatus::Active);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn reactivate_clears_failed_for_refetch() {
        let mem = MemResource::new(CancellationToken::default());
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
