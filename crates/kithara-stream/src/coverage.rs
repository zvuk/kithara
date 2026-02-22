#![forbid(unsafe_code)]

use std::{fmt::Debug, hash::Hash, sync::Arc};

use kithara_assets::{Assets, AssetsBackend, AssetsError};
use kithara_coverage::{CoverageIndex, DiskCoverage};
use kithara_storage::StorageResource;

pub type CoverageIndexHandle = CoverageIndex<StorageResource>;
pub type CoverageState = DiskCoverage<StorageResource>;

#[derive(Clone)]
pub struct CoverageManager {
    index: Arc<CoverageIndexHandle>,
}

impl CoverageManager {
    #[must_use]
    pub fn from_index(index: Arc<CoverageIndexHandle>) -> Self {
        Self { index }
    }

    #[must_use]
    pub fn index(&self) -> Arc<CoverageIndexHandle> {
        Arc::clone(&self.index)
    }

    #[must_use]
    pub fn open_state<K>(&self, key: K) -> CoverageState
    where
        K: Into<String>,
    {
        CoverageState::open(self.index(), key.into())
    }

    /// Open coverage manager for the provided assets backend.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` when coverage index resource cannot be opened.
    pub fn open<Ctx>(backend: &AssetsBackend<Ctx>) -> Result<Self, AssetsError>
    where
        Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
    {
        Ok(Self::from_index(open_coverage_index(backend)?))
    }
}

/// Open coverage index storage for the provided assets backend.
///
/// # Errors
///
/// Returns `AssetsError` if the backend cannot open coverage index resource.
pub fn open_coverage_index<Ctx>(
    backend: &AssetsBackend<Ctx>,
) -> Result<Arc<CoverageIndexHandle>, AssetsError>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    let res: StorageResource = match backend {
        #[cfg(not(target_arch = "wasm32"))]
        AssetsBackend::Disk(store) => store.open_coverage_index_resource()?.into(),
        AssetsBackend::Mem(store) => store.open_coverage_index_resource()?.into(),
    };
    Ok(Arc::new(CoverageIndex::new(res)))
}
