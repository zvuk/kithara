//! Probe / direct-reader bootstrap parameters.
//!
//! Used by [`super::probe::new_direct`] / [`super::probe::probe_with_seek`]
//! to wire a Symphonia format reader. The original "decoder god-type"
//! configuration (gapless, verify, `stream_ctx`, epoch, `pcm_pool`) was
//! removed when the only consumer — `SymphoniaDecoder` — was deleted.

use std::sync::{Arc, atomic::AtomicU64};

/// Minimal configuration carried into the probe / direct-reader path.
#[derive(Default)]
pub(crate) struct SymphoniaConfig {
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// File extension hint for Symphonia probe (e.g., `"mp3"`, `"aac"`).
    ///
    /// Used by the probe path when no container is known up-front.
    pub hint: Option<String>,
    /// Enable gapless trim wiring through the Symphonia decoder.
    ///
    /// When `true`, [`SymphoniaCodec::open_with_config`](super::codec::SymphoniaCodec::open_with_config)
    /// flips `AudioDecoderOptions::gapless = true`, and the factory in
    /// P7 calls [`probe_track_info`](super::codec::SymphoniaCodec::probe_track_info)
    /// to populate [`crate::DecoderTrackInfo::gapless`] from MP4 udta.
    pub gapless: bool,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn test_symphonia_config_default() {
        let config = SymphoniaConfig::default();
        assert!(config.byte_len_handle.is_none());
        assert!(config.hint.is_none());
        assert!(!config.gapless);
    }

    #[kithara::test]
    fn test_symphonia_config_with_hint() {
        let config = SymphoniaConfig {
            hint: Some("mp3".into()),
            ..Default::default()
        };
        assert_eq!(config.hint, Some("mp3".into()));
    }

    #[kithara::test]
    fn test_symphonia_config_with_gapless() {
        let config = SymphoniaConfig {
            gapless: true,
            ..Default::default()
        };
        assert!(config.gapless);
    }
}
