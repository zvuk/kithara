use kithara_stream::{AudioCodec, ContainerFormat};
use kithara_test_utils::kithara;

use super::protocol::{BoxedSource, HardwareBackend, hardware_accepts};
use crate::{DecodeError, DecoderConfig, InnerDecoder};

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
    fn can_seek_container(_container: ContainerFormat) -> bool {
        false
    }

    fn default_container_for_codec(_codec: AudioCodec) -> Option<ContainerFormat> {
        None
    }

    fn supports_codec(_codec: AudioCodec) -> bool {
        false
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
    fn can_seek_container(container: ContainerFormat) -> bool {
        container == ContainerFormat::MpegAudio
    }

    fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
        (codec == AudioCodec::Mp3).then_some(ContainerFormat::MpegAudio)
    }

    fn supports_codec(codec: AudioCodec) -> bool {
        codec == AudioCodec::Mp3
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
    let accepted =
        hardware_accepts::<UnsupportedBackend>(AudioCodec::Mp3, Some(ContainerFormat::MpegAudio));
    assert!(accepted.is_none());
}

#[kithara::test]
fn test_hardware_accepts_uses_explicit_container_when_supported() {
    let accepted =
        hardware_accepts::<Mp3CapabilityBackend>(AudioCodec::Mp3, Some(ContainerFormat::MpegAudio));
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
