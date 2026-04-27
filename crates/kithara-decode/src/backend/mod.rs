//! Hardware-backend protocol shared by Apple `AudioToolbox` and
//! Android `MediaCodec`.
//!
//! Each per-platform impl lives alongside its backend module
//! ([`crate::apple::AppleBackend`], [`crate::android::AndroidBackend`]);
//! the factory picks one through [`current::Current`] (see
//! [`current`]).

pub(crate) mod current;

use kithara_stream::{AudioCodec, ContainerFormat};

use crate::{DecodeError, DecoderConfig, InnerDecoder, traits::DecoderInput};

pub(crate) type BoxedSource = Box<dyn DecoderInput>;

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
pub(crate) trait HardwareBackend {
    /// Whether this backend can decode `codec`.
    fn supports_codec(codec: AudioCodec) -> bool;

    /// Whether this backend can reliably seek within `container`.
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
    ) -> Result<Box<dyn InnerDecoder>, DecodeError>;
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
    ) -> Result<Box<dyn InnerDecoder>, DecodeError> {
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
        ) -> Result<Box<dyn InnerDecoder>, DecodeError> {
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
        ) -> Result<Box<dyn InnerDecoder>, DecodeError> {
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
