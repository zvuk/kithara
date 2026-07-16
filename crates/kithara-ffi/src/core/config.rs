use std::fmt;

use kithara_platform::sync::Arc;

use crate::layout::FfiAssetLayout;

/// Protocol whose resources use a registered cache layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FfiCacheLayoutTarget {
    /// Direct-file sources and their derived resources.
    File,
    /// HLS playlists, media resources, keys, and derived resources.
    Hls,
}

/// One protocol-specific cache layout registration.
#[derive(Clone)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiCacheLayoutRegistration {
    pub target: FfiCacheLayoutTarget,
    pub layout: Arc<dyn FfiAssetLayout>,
}

impl fmt::Debug for FfiCacheLayoutRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FfiCacheLayoutRegistration")
            .field("target", &self.target)
            .field("layout", &"...")
            .finish()
    }
}

/// Cache configuration shared by all resources created by one player.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiCacheConfig {
    /// Outer directory containing the entire asset store.
    pub cache_dir: Option<String>,
    /// Protocol-specific layouts. Later registrations replace earlier ones.
    pub layouts: Vec<FfiCacheLayoutRegistration>,
}
