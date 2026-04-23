//! Configuration for the Apple `AudioToolbox` decoder.

use std::sync::{Arc, atomic::AtomicU64};

use kithara_bufpool::PcmPool;
use kithara_stream::ContainerFormat;

/// Configuration for Apple `AudioToolbox` decoder.
#[derive(Debug, Clone, Default)]
pub(crate) struct AppleConfig {
    /// Handle for dynamic byte length updates (HLS).
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    /// Container format hint for file type detection.
    pub(crate) container: Option<ContainerFormat>,
    /// Optional PCM buffer pool override.
    ///
    /// When `None`, the global `kithara_bufpool::pcm_pool()` is used.
    pub(crate) pcm_pool: Option<PcmPool>,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case::default(None)]
    #[case::fmp4(Some(ContainerFormat::Fmp4))]
    fn test_apple_config_container(#[case] container: Option<ContainerFormat>) {
        let config = AppleConfig {
            container,
            pcm_pool: None,
            ..Default::default()
        };
        assert!(config.byte_len_handle.is_none());
        assert_eq!(config.container, container);
        assert!(config.pcm_pool.is_none());
    }
}
