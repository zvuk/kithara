use kithara_platform::sync::Arc;

use crate::{asset::FfiAssetStore, types::FfiKeyOptions};

/// FFI-friendly player configuration.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiPlayerConfig {
    /// DRM key handling. Pass an empty [`FfiKeyOptions`] when no DRM is needed.
    pub key_options: FfiKeyOptions,
    /// Shared asset store used by every item created by this player.
    pub store: Arc<FfiAssetStore>,
    /// Number of EQ bands (log-spaced). Default: 10.
    pub eq_band_count: u32,
}

impl Default for FfiPlayerConfig {
    fn default() -> Self {
        Self {
            eq_band_count: 10,
            key_options: FfiKeyOptions::default(),
            store: Arc::new(FfiAssetStore::default()),
        }
    }
}
