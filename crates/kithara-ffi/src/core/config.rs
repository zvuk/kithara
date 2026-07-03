use std::{fmt, sync::Arc};

use crate::layout::FfiAssetLayout;

/// Store configuration forwarded from platform layer to resource creation.
#[derive(Clone, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct StoreOptions {
    pub cache_dir: Option<String>,
    /// Foreign on-disk layout delegate. `None` == the store's default layout.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub layout: Option<Arc<dyn FfiAssetLayout>>,
}

impl fmt::Debug for StoreOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreOptions")
            .field("cache_dir", &self.cache_dir)
            .field("layout", &self.layout.as_ref().map(|_| "..."))
            .finish()
    }
}
