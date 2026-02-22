#![forbid(unsafe_code)]

use std::{fmt::Debug, hash::Hash, sync::Arc};

use kithara_assets::{Assets, AssetsBackend, AssetsError, CoverageIndex, DiskCoverage};
use kithara_storage::StorageResource;

pub type CoverageIndexHandle = CoverageIndex<StorageResource>;
pub type CoverageState = DiskCoverage<StorageResource>;

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
