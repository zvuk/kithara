#![forbid(unsafe_code)]

//! Unified storage resource: disk (mmap) or memory backend.

use std::{ops::Range, path::Path};

use crate::{
    MemResource, MmapResource, StorageResult,
    resource::{ResourceExt, ResourceStatus, WaitOutcome},
};

/// Unified resource: disk (mmap) or memory backend.
///
/// Dispatches all [`ResourceExt`] operations to the inner variant.
/// Use [`From<MmapResource>`] or [`From<MemResource>`] for construction.
#[derive(Clone, Debug)]
pub enum StorageResource {
    /// File-backed mmap resource.
    Mmap(MmapResource),
    /// In-memory resource.
    Mem(MemResource),
}

impl From<MmapResource> for StorageResource {
    fn from(r: MmapResource) -> Self {
        Self::Mmap(r)
    }
}

impl From<MemResource> for StorageResource {
    fn from(r: MemResource) -> Self {
        Self::Mem(r)
    }
}

impl ResourceExt for StorageResource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        match self {
            Self::Mmap(r) => r.read_at(offset, buf),
            Self::Mem(r) => r.read_at(offset, buf),
        }
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        match self {
            Self::Mmap(r) => r.write_at(offset, data),
            Self::Mem(r) => r.write_at(offset, data),
        }
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        match self {
            Self::Mmap(r) => r.wait_range(range),
            Self::Mem(r) => r.wait_range(range),
        }
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        match self {
            Self::Mmap(r) => r.commit(final_len),
            Self::Mem(r) => r.commit(final_len),
        }
    }

    fn fail(&self, reason: String) {
        match self {
            Self::Mmap(r) => r.fail(reason),
            Self::Mem(r) => r.fail(reason),
        }
    }

    fn path(&self) -> Option<&Path> {
        match self {
            Self::Mmap(r) => r.path(),
            Self::Mem(r) => r.path(),
        }
    }

    fn len(&self) -> Option<u64> {
        match self {
            Self::Mmap(r) => r.len(),
            Self::Mem(r) => r.len(),
        }
    }

    fn reactivate(&self) -> StorageResult<()> {
        match self {
            Self::Mmap(r) => r.reactivate(),
            Self::Mem(r) => r.reactivate(),
        }
    }

    fn status(&self) -> ResourceStatus {
        match self {
            Self::Mmap(r) => r.status(),
            Self::Mem(r) => r.status(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::rstest;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{MmapOptions, OpenMode, Resource};

    #[rstest]
    #[timeout(Duration::from_secs(5))]
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

    #[rstest]
    #[timeout(Duration::from_secs(5))]
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

    #[rstest]
    #[timeout(Duration::from_secs(5))]
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

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn from_mem_resource() {
        let mem = MemResource::new(CancellationToken::new());
        let res: StorageResource = mem.into();
        assert!(matches!(res, StorageResource::Mem(_)));
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn status_delegation() {
        let mem = MemResource::new(CancellationToken::new());
        let res = StorageResource::from(mem);

        assert_eq!(res.status(), ResourceStatus::Active);
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn wait_range_delegation() {
        let mem = MemResource::new(CancellationToken::new());
        let res = StorageResource::from(mem);

        res.write_at(0, b"data").unwrap();
        let outcome = res.wait_range(0..4).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn fail_delegation() {
        let mem = MemResource::new(CancellationToken::new());
        let res = StorageResource::from(mem);

        res.fail("boom".to_string());
        assert_eq!(res.status(), ResourceStatus::Failed("boom".to_string()));
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
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
