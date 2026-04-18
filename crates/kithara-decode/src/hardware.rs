use std::io::{Seek, SeekFrom};

use kithara_stream::{AudioCodec, ContainerFormat};
#[cfg(any(
    test,
    all(feature = "android", target_os = "android"),
    all(feature = "apple", any(target_os = "macos", target_os = "ios"))
))]
use tracing::warn;

#[cfg(all(feature = "android", target_os = "android"))]
use crate::android::{
    AndroidConfig, can_seek_container as android_can_seek_container,
    default_container_for_codec as android_default_container_for_codec,
    supports_codec as android_supports_codec, try_create_android_decoder,
};
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use crate::apple::{AppleConfig, try_create_apple_decoder};
use crate::{DecodeError, DecoderConfig, InnerDecoder, traits::DecoderInput};

pub(crate) type BoxedSource = Box<dyn DecoderInput>;

pub(crate) struct RecoverableHardwareError {
    pub(crate) source: BoxedSource,
    pub(crate) error: DecodeError,
}

#[cfg(any(
    test,
    all(feature = "android", target_os = "android"),
    all(feature = "apple", any(target_os = "macos", target_os = "ios"))
))]
pub(crate) fn recoverable_hardware_error(
    mut source: BoxedSource,
    error: DecodeError,
) -> RecoverableHardwareError {
    if let Err(rewind_error) = Seek::seek(&mut *source, SeekFrom::Start(0)) {
        warn!(
            ?error,
            ?rewind_error,
            "Hardware backend failed and source rewind for Symphonia fallback also failed"
        );
    }

    RecoverableHardwareError { source, error }
}

/// Check whether a hardware backend accepts this codec/container pair.
///
/// Returns the resolved container (possibly inferred from codec) when the
/// backend can handle it.  Returns `None` when the backend should be
/// skipped — the caller keeps `source` and falls through to Symphonia.
pub(crate) fn hardware_accepts<B: HardwareBackend>(
    codec: AudioCodec,
    container: Option<ContainerFormat>,
) -> Option<ContainerFormat> {
    if !B::supports_codec(codec) {
        return None;
    }
    let resolved = container.or_else(|| B::default_container_for_codec(codec))?;
    if !B::can_seek_container(resolved) {
        return None;
    }
    Some(resolved)
}

/// Capability description for a platform-specific hardware decoder backend.
///
/// Each backend (Apple `AudioToolbox`, Android `MediaCodec`, etc.) implements
/// this trait so the factory can query codec/container support uniformly.
pub(crate) trait HardwareBackend {
    /// Whether this backend can decode `codec`.
    fn supports_codec(codec: AudioCodec) -> bool;

    /// Whether this backend can reliably seek within `container`.
    ///
    /// Frame-based formats (MP3, ADTS-AAC, raw FLAC) have self-delimiting
    /// sync patterns that allow byte-level seeking.  Structured containers
    /// (fMP4, MPEG-TS) need atom-boundary awareness the backend may lack.
    fn can_seek_container(container: ContainerFormat) -> bool;

    /// Infer the most likely container for `codec` when metadata doesn't
    /// supply one (e.g. codec known from file extension only).
    fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat>;

    /// Create a decoder for the given codec/container pair.
    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError>;
}

/// Canonical hardware backend entrypoint for the current build target.
///
/// The concrete backend remains behind the `HardwareBackend` trait so tests can
/// still inject custom implementations, but production code no longer needs a
/// dummy `NoHardwareBackend` placeholder just to satisfy type selection.
pub(crate) struct PlatformBackend;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod platform {
    use kithara_stream::{AudioCodec, ContainerFormat};

    use super::{AppleBackend, BoxedSource, HardwareBackend, RecoverableHardwareError};
    use crate::{DecoderConfig, InnerDecoder};

    pub(super) fn supports_codec(codec: AudioCodec) -> bool {
        AppleBackend::supports_codec(codec)
    }

    pub(super) fn can_seek_container(container: ContainerFormat) -> bool {
        AppleBackend::can_seek_container(container)
    }

    pub(super) fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
        AppleBackend::default_container_for_codec(codec)
    }

    pub(super) fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        AppleBackend::try_create(source, config, codec, container)
    }
}

#[cfg(all(feature = "android", target_os = "android"))]
mod platform {
    use kithara_stream::{AudioCodec, ContainerFormat};

    use super::{AndroidBackend, BoxedSource, HardwareBackend, RecoverableHardwareError};
    use crate::{DecoderConfig, InnerDecoder};

    pub(super) fn supports_codec(codec: AudioCodec) -> bool {
        AndroidBackend::supports_codec(codec)
    }

    pub(super) fn can_seek_container(container: ContainerFormat) -> bool {
        AndroidBackend::can_seek_container(container)
    }

    pub(super) fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
        AndroidBackend::default_container_for_codec(codec)
    }

    pub(super) fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        AndroidBackend::try_create(source, config, codec, container)
    }
}

#[cfg(not(any(
    all(feature = "apple", any(target_os = "macos", target_os = "ios")),
    all(feature = "android", target_os = "android")
)))]
mod platform {
    use kithara_stream::{AudioCodec, ContainerFormat};

    use super::{BoxedSource, RecoverableHardwareError};
    use crate::{DecoderConfig, InnerDecoder};

    pub(super) fn supports_codec(_codec: AudioCodec) -> bool {
        false
    }

    pub(super) fn can_seek_container(_container: ContainerFormat) -> bool {
        false
    }

    pub(super) fn default_container_for_codec(_codec: AudioCodec) -> Option<ContainerFormat> {
        None
    }

    pub(super) fn try_create(
        _source: BoxedSource,
        _config: &DecoderConfig,
        _codec: AudioCodec,
        _container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        // `PlatformBackend::try_create` is only reached after `hardware_accepts`
        // has confirmed that the active platform advertises support.
        unreachable!("PlatformBackend::try_create is gated by hardware_accepts")
    }
}

impl HardwareBackend for PlatformBackend {
    fn supports_codec(codec: AudioCodec) -> bool {
        platform::supports_codec(codec)
    }

    fn can_seek_container(container: ContainerFormat) -> bool {
        platform::can_seek_container(container)
    }

    fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
        platform::default_container_for_codec(codec)
    }

    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        platform::try_create(source, config, codec, container)
    }
}

// Apple AudioToolbox backend
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
pub(crate) struct AppleBackend;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
impl HardwareBackend for AppleBackend {
    fn supports_codec(codec: AudioCodec) -> bool {
        matches!(
            codec,
            AudioCodec::AacLc
                | AudioCodec::AacHe
                | AudioCodec::AacHeV2
                | AudioCodec::Mp3
                | AudioCodec::Flac
                | AudioCodec::Alac
        )
    }

    fn can_seek_container(container: ContainerFormat) -> bool {
        // Frame-based formats have self-delimiting sync patterns.
        // Structured containers (fMP4, MPEG-TS, CAF) require
        // atom-boundary awareness that AudioFileStream lacks.
        matches!(
            container,
            ContainerFormat::MpegAudio | ContainerFormat::Adts | ContainerFormat::Flac
        )
    }

    fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
        match codec {
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
                Some(ContainerFormat::Adts)
            }
            AudioCodec::Mp3 => Some(ContainerFormat::MpegAudio),
            AudioCodec::Flac => Some(ContainerFormat::Flac),
            AudioCodec::Alac => Some(ContainerFormat::Fmp4),
            _ => None,
        }
    }

    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        let apple_config = AppleConfig {
            byte_len_handle: config.byte_len_handle.clone(),
            container,
            pcm_pool: config.pcm_pool.clone(),
        };

        try_create_apple_decoder(codec, source, &apple_config)
    }
}

// Android MediaCodec backend
#[cfg(all(feature = "android", target_os = "android"))]
pub(crate) struct AndroidBackend;

#[cfg(all(feature = "android", target_os = "android"))]
impl HardwareBackend for AndroidBackend {
    fn supports_codec(codec: AudioCodec) -> bool {
        android_supports_codec(codec)
    }

    fn can_seek_container(container: ContainerFormat) -> bool {
        android_can_seek_container(container)
    }

    fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
        android_default_container_for_codec(codec)
    }

    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        let android_config = AndroidConfig {
            byte_len_handle: config.byte_len_handle.clone(),
            container,
            pcm_pool: config.pcm_pool.clone(),
            stream_ctx: config.stream_ctx.clone(),
            epoch: config.epoch,
        };

        try_create_android_decoder(codec, source, android_config)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn unsupported_try_create(
        _source: BoxedSource,
        _config: &DecoderConfig,
        _codec: AudioCodec,
        _container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        unreachable!("capability-only test backend should never construct a decoder")
    }

    struct UnsupportedBackend;
    struct Mp3CapabilityBackend;

    impl HardwareBackend for UnsupportedBackend {
        fn supports_codec(_codec: AudioCodec) -> bool {
            false
        }

        fn can_seek_container(_container: ContainerFormat) -> bool {
            false
        }

        fn default_container_for_codec(_codec: AudioCodec) -> Option<ContainerFormat> {
            None
        }

        fn try_create(
            source: BoxedSource,
            config: &DecoderConfig,
            codec: AudioCodec,
            container: Option<ContainerFormat>,
        ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
            unsupported_try_create(source, config, codec, container)
        }
    }

    impl HardwareBackend for Mp3CapabilityBackend {
        fn supports_codec(codec: AudioCodec) -> bool {
            codec == AudioCodec::Mp3
        }

        fn can_seek_container(container: ContainerFormat) -> bool {
            container == ContainerFormat::MpegAudio
        }

        fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
            (codec == AudioCodec::Mp3).then_some(ContainerFormat::MpegAudio)
        }

        fn try_create(
            source: BoxedSource,
            config: &DecoderConfig,
            codec: AudioCodec,
            container: Option<ContainerFormat>,
        ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
            unsupported_try_create(source, config, codec, container)
        }
    }

    #[kithara::test]
    fn test_hardware_accepts_rejects_backends_without_capabilities() {
        let accepted = hardware_accepts::<UnsupportedBackend>(
            AudioCodec::Mp3,
            Some(ContainerFormat::MpegAudio),
        );
        assert!(accepted.is_none());
    }

    #[kithara::test]
    fn test_hardware_accepts_uses_explicit_container_when_supported() {
        let accepted = hardware_accepts::<Mp3CapabilityBackend>(
            AudioCodec::Mp3,
            Some(ContainerFormat::MpegAudio),
        );
        assert_eq!(accepted, Some(ContainerFormat::MpegAudio));
    }

    #[kithara::test]
    fn test_hardware_accepts_infers_container_from_codec() {
        let accepted = hardware_accepts::<Mp3CapabilityBackend>(AudioCodec::Mp3, None);
        assert_eq!(accepted, Some(ContainerFormat::MpegAudio));
    }

    #[kithara::test]
    fn test_hardware_accepts_rejects_unsupported_container() {
        let accepted =
            hardware_accepts::<Mp3CapabilityBackend>(AudioCodec::Mp3, Some(ContainerFormat::Fmp4));
        assert!(accepted.is_none());
    }
}
