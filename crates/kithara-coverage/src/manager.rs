#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_storage::ResourceExt;

use crate::{CoverageIndex, DiskCoverage};

#[derive(Clone)]
pub struct CoverageManager<R: ResourceExt> {
    index: Arc<CoverageIndex<R>>,
}

impl<R: ResourceExt> CoverageManager<R> {
    #[must_use]
    pub fn from_index(index: Arc<CoverageIndex<R>>) -> Self {
        Self { index }
    }

    #[must_use]
    pub fn index(&self) -> Arc<CoverageIndex<R>> {
        Arc::clone(&self.index)
    }

    #[must_use]
    pub fn open_state<K>(&self, key: K) -> DiskCoverage<R>
    where
        K: Into<String>,
    {
        DiskCoverage::open(self.index(), key.into())
    }
}
