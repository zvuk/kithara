#![forbid(unsafe_code)]

//! Unified storage resource: disk (mmap) or memory backend.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(not(target_arch = "wasm32"))]
use crate::MmapResource;
use crate::{
    AtomicChunked, MemResource, StorageResult,
    resource::{ResourceExt, ResourceStatus, WaitOutcome},
};

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
    Mmap(Arc<AtomicChunked<MmapResource>>),
    /// In-memory resource (always passthrough — memory has no
    /// torn-write hazard).
    Mem(Arc<AtomicChunked<MemResource>>),
}

#[cfg(not(target_arch = "wasm32"))]
impl From<MmapResource> for StorageResource {
    fn from(r: MmapResource) -> Self {
        // `MmapResource::path()` is always `Some` (file-backed). Use
        // it as the canonical path for the passthrough decorator.
        let path = r.path().map(Path::to_path_buf).unwrap_or_default();
        Self::Mmap(Arc::new(AtomicChunked::passthrough(r, path)))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<AtomicChunked<MmapResource>> for StorageResource {
    fn from(r: AtomicChunked<MmapResource>) -> Self {
        Self::Mmap(Arc::new(r))
    }
}

impl From<MemResource> for StorageResource {
    fn from(r: MemResource) -> Self {
        // Memory inners have no filesystem path. The passthrough
        // canonical path is meaningful only when `path()` is queried,
        // and for mem we want it to remain `None`. Use an empty
        // PathBuf as placeholder; `StorageResource::path()` overrides
        // for the Mem variant to return `None` regardless.
        Self::Mem(Arc::new(AtomicChunked::passthrough(r, PathBuf::default())))
    }
}

impl ResourceExt for StorageResource {
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.commit(final_len),
            Self::Mem(r) => r.commit(final_len),
        }
    }

    fn contains_range(&self, range: Range<u64>) -> bool {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.contains_range(range),
            Self::Mem(r) => r.contains_range(range),
        }
    }

    fn fail(&self, reason: String) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.fail(reason),
            Self::Mem(r) => r.fail(reason),
        }
    }

    fn len(&self) -> Option<u64> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.len(),
            Self::Mem(r) => r.len(),
        }
    }

    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.next_gap(from, limit),
            Self::Mem(r) => r.next_gap(from, limit),
        }
    }

    fn path(&self) -> Option<&Path> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.path(),
            // Memory inners have no filesystem path; the passthrough
            // wrapper carries an empty placeholder we never expose.
            Self::Mem(_) => None,
        }
    }

    fn reactivate(&self) -> StorageResult<()> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.reactivate(),
            Self::Mem(r) => r.reactivate(),
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.read_at(offset, buf),
            Self::Mem(r) => r.read_at(offset, buf),
        }
    }

    fn status(&self) -> ResourceStatus {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.status(),
            Self::Mem(r) => r.status(),
        }
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Mmap(r) => r.wait_range(range),
            Self::Mem(r) => r.wait_range(range),
        }
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
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

    use kithara_platform::time::Duration;
    use tokio_util::sync::CancellationToken;

    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    use crate::{MmapOptions, OpenMode, Resource};

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn mmap_variant_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mmap: MmapResource = Resource::open(
            CancellationToken::new(),
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
        let mem = MemResource::new(CancellationToken::new());
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
            CancellationToken::new(),
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
        let mem = MemResource::new(CancellationToken::new());
        let res: StorageResource = mem.into();
        assert!(matches!(res, StorageResource::Mem(_)));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn status_delegation() {
        let mem = MemResource::new(CancellationToken::new());
        let res = StorageResource::from(mem);

        assert_eq!(res.status(), ResourceStatus::Active);
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn wait_range_delegation() {
        let mem = MemResource::new(CancellationToken::new());
        let res = StorageResource::from(mem);

        res.write_at(0, b"data").unwrap();
        let outcome = res.wait_range(0..4).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn fail_delegation() {
        let mem = MemResource::new(CancellationToken::new());
        let res = StorageResource::from(mem);

        res.fail("boom".to_string());
        assert_eq!(res.status(), ResourceStatus::Failed("boom".to_string()));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn reactivate_delegation() {
        let mem = MemResource::new(CancellationToken::new());
        let res = StorageResource::from(mem);

        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

        res.reactivate().unwrap();
        assert_eq!(res.status(), ResourceStatus::Active);
    }
}
