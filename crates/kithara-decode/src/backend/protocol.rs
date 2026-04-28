//! Hardware-backend protocol shared by Apple `AudioToolbox` and
//! Android `MediaCodec`.

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
    /// Whether this backend can reliably seek within `container`.
    fn can_seek_container(container: ContainerFormat) -> bool;

    /// Infer the most likely container for `codec` when metadata doesn't
    /// supply one (e.g. codec known from file extension only).
    fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat>;

    /// Whether this backend can decode `codec`.
    fn supports_codec(codec: AudioCodec) -> bool;

    /// Create a decoder for the given codec/container pair.
    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, DecodeError>;
}
