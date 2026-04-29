//! Configuration for Symphonia-based decoders.

use std::sync::{Arc, atomic::AtomicU64};

use kithara_bufpool::PcmPool;
use kithara_stream::{ContainerFormat, StreamContext};

/// Configuration for Symphonia-based decoders.
#[derive(Default)]
pub(crate) struct SymphoniaConfig {
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Container format for direct reader creation (no probe).
    ///
    /// When set, bypasses Symphonia's probe mechanism and creates
    /// the appropriate format reader directly. This is critical for
    /// HLS streams where the container format is known from playlist.
    ///
    /// When not set, falls back to Symphonia's probe mechanism which
    /// can automatically detect format and skip junk data.
    pub container: Option<ContainerFormat>,
    /// File extension hint for Symphonia probe (e.g., "mp3", "aac").
    ///
    /// Used only when `container` is not set, as a hint for the probe.
    pub hint: Option<String>,
    /// Optional PCM buffer pool override.
    ///
    /// When `None`, the global `kithara_bufpool::pcm_pool()` is used.
    pub pcm_pool: Option<PcmPool>,
    /// Stream context for segment/variant metadata.
    pub stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Enable gapless playback.
    pub gapless: bool,
    /// Enable data verification (slower but safer).
    pub verify: bool,
    /// Epoch counter for decoder recreation tracking.
    pub epoch: u64,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn test_symphonia_config_default() {
        let config = SymphoniaConfig::default();
        assert!(!config.verify);
        assert!(!config.gapless);
        assert!(config.byte_len_handle.is_none());
        assert!(config.container.is_none());
        assert!(config.hint.is_none());
    }

    #[kithara::test]
    fn test_symphonia_config_with_container() {
        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Fmp4),
            ..Default::default()
        };
        assert_eq!(config.container, Some(ContainerFormat::Fmp4));
    }

    #[kithara::test]
    fn test_config_with_custom_handle() {
        let handle = Arc::new(AtomicU64::new(12345));
        let config = SymphoniaConfig {
            verify: true,
            gapless: false,
            byte_len_handle: Some(Arc::clone(&handle)),
            container: Some(ContainerFormat::Fmp4),
            hint: Some("mp4".to_string()),
            stream_ctx: None,
            epoch: 0,
            pcm_pool: None,
        };

        assert!(config.verify);
        assert!(!config.gapless);
        assert!(config.byte_len_handle.is_some());
        assert_eq!(config.container, Some(ContainerFormat::Fmp4));
        assert_eq!(config.hint, Some("mp4".to_string()));
    }
}
