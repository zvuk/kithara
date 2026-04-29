//! Configuration for the Apple `AudioToolbox` decoder.

use std::sync::{Arc, atomic::AtomicU64};

use kithara_bufpool::{BytePool, PcmPool};
use kithara_stream::{ContainerFormat, StreamContext};

/// Configuration for Apple `AudioToolbox` decoder.
#[derive(Clone, Default)]
pub(crate) struct AppleConfig {
    /// Handle for dynamic byte length updates (HLS).
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    /// Optional byte buffer pool for the fMP4 reader's backing storage.
    ///
    /// When `None`, the global `kithara_bufpool::byte_pool()` is used.
    pub(crate) byte_pool: Option<BytePool>,
    /// Container format hint for file type detection.
    pub(crate) container: Option<ContainerFormat>,
    /// Optional PCM buffer pool override.
    ///
    /// When `None`, the global `kithara_bufpool::pcm_pool()` is used.
    pub(crate) pcm_pool: Option<PcmPool>,
    /// Stream context for segment/variant metadata propagation into
    /// emitted [`PcmMeta`](crate::types::PcmMeta).
    pub(crate) stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Epoch counter for decoder recreation tracking, mirrored into
    /// [`PcmMeta::epoch`](crate::types::PcmMeta).
    pub(crate) epoch: u64,
}

impl std::fmt::Debug for AppleConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppleConfig")
            .field("byte_len_handle", &self.byte_len_handle)
            .field("byte_pool", &self.byte_pool)
            .field("container", &self.container)
            .field("pcm_pool", &self.pcm_pool)
            .field("stream_ctx", &self.stream_ctx.is_some())
            .field("epoch", &self.epoch)
            .finish()
    }
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
