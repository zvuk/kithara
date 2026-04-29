use std::time::Duration;

use kithara_stream::{AudioCodec, ContainerFormat};
use kithara_test_utils::kithara;

use super::protocol::{Backend, BoxedSource};
use crate::{
    DecoderConfig,
    error::DecodeResult,
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    types::{PcmSpec, TrackMetadata},
};

/// Capability-only test backends. They satisfy `Backend: Decoder` so the
/// trait dispatch shape is exercised, but their runtime methods panic if
/// ever invoked — these tests never construct an actual decoder.
struct UnsupportedBackend;
struct Mp3OnlyBackend;
struct ProbingBackend;

macro_rules! impl_unreachable_decoder {
    ($t:ty) => {
        impl Decoder for $t {
            fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
                unreachable!("capability-only test backend should never decode")
            }
            fn spec(&self) -> PcmSpec {
                unreachable!("capability-only test backend has no spec")
            }
            fn seek(&mut self, _pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
                unreachable!("capability-only test backend should never seek")
            }
            fn update_byte_len(&self, _len: u64) {
                unreachable!("capability-only test backend has no byte-len handle")
            }
            fn duration(&self) -> Option<Duration> {
                unreachable!("capability-only test backend has no duration")
            }
            fn metadata(&self) -> TrackMetadata {
                unreachable!("capability-only test backend has no metadata")
            }
        }
    };
}

impl_unreachable_decoder!(UnsupportedBackend);
impl_unreachable_decoder!(Mp3OnlyBackend);
impl_unreachable_decoder!(ProbingBackend);

impl Backend for UnsupportedBackend {
    fn supports(_codec: AudioCodec, _container: Option<ContainerFormat>) -> bool {
        false
    }

    fn try_create(
        _source: BoxedSource,
        _config: &DecoderConfig,
        _codec: AudioCodec,
        _container: Option<ContainerFormat>,
    ) -> DecodeResult<Self> {
        unreachable!("capability-only test backend should never construct a decoder")
    }
}

impl Backend for Mp3OnlyBackend {
    fn supports(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
        if codec != AudioCodec::Mp3 {
            return false;
        }
        match container {
            Some(ContainerFormat::MpegAudio) | None => true,
            Some(_) => false,
        }
    }

    fn try_create(
        _source: BoxedSource,
        _config: &DecoderConfig,
        _codec: AudioCodec,
        _container: Option<ContainerFormat>,
    ) -> DecodeResult<Self> {
        unreachable!("capability-only test backend should never construct a decoder")
    }
}

impl Backend for ProbingBackend {
    fn supports(_codec: AudioCodec, _container: Option<ContainerFormat>) -> bool {
        true
    }

    fn try_create(
        _source: BoxedSource,
        _config: &DecoderConfig,
        _codec: AudioCodec,
        _container: Option<ContainerFormat>,
    ) -> DecodeResult<Self> {
        unreachable!("capability-only test backend should never construct a decoder")
    }
}

#[kithara::test]
fn supports_returns_false_when_backend_lacks_capability() {
    assert!(!UnsupportedBackend::supports(
        AudioCodec::Mp3,
        Some(ContainerFormat::MpegAudio)
    ));
}

#[kithara::test]
fn supports_accepts_explicit_matching_container() {
    assert!(Mp3OnlyBackend::supports(
        AudioCodec::Mp3,
        Some(ContainerFormat::MpegAudio)
    ));
}

#[kithara::test]
fn supports_accepts_missing_container_when_codec_alone_is_enough() {
    assert!(Mp3OnlyBackend::supports(AudioCodec::Mp3, None));
}

#[kithara::test]
fn supports_rejects_explicit_unsupported_container() {
    assert!(!Mp3OnlyBackend::supports(
        AudioCodec::Mp3,
        Some(ContainerFormat::Fmp4)
    ));
}

#[kithara::test]
fn probing_backend_accepts_missing_container() {
    assert!(ProbingBackend::supports(AudioCodec::AacLc, None));
}
