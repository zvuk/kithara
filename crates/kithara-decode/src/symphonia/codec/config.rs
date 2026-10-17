use std::sync::atomic::AtomicU64;

use kithara_config::Config;
use kithara_platform::sync::Arc;
use kithara_stream::ReaderControl;

/// Minimal configuration carried into the probe / direct-reader path.
#[derive(Default, Config)]
#[config(construction)]
pub(crate) struct SymphoniaConfig {
    /// Handle for dynamic byte length updates (HLS).
    #[config(skip = "transferred to the Symphonia reader")]
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    #[config(skip = "transferred to the native parser input")]
    pub(crate) reader_control: Option<ReaderControl>,
    /// File extension hint for Symphonia probe (e.g., `"mp3"`, `"aac"`).
    ///
    /// Used by the probe path when no container is known up-front.
    #[config(skip = "consumed by the probe", builder(into))]
    pub(crate) hint: Option<String>,
    /// Enable gapless trim wiring through the Symphonia decoder.
    ///
    /// When `true`, [`SymphoniaCodec::open_with_config`](super::SymphoniaCodec::open_with_config)
    /// flips `AudioDecoderOptions::gapless = true` so codecs that
    /// internally trim (FLAC/Opus/Vorbis) honour it. MP3 priming is
    /// applied separately via
    /// [`crate::codec::FrameCodec::decoder_algo_delay`].
    #[config(value, builder(default))]
    pub(crate) gapless: bool,
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
        let config = SymphoniaConfig::builder().hint("mp3").build();
        assert_eq!(config.hint, Some("mp3".into()));
    }

    #[kithara::test]
    fn test_symphonia_config_with_gapless() {
        let config = SymphoniaConfig::builder().gapless(true).build();
        assert!(config.gapless);
    }
}
