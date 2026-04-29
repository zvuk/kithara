//! Backend protocol shared by all decoder backends (Apple `AudioToolbox`,
//! Android `MediaCodec`, Symphonia software).

use kithara_stream::{AudioCodec, ContainerFormat};

use crate::{
    DecoderConfig,
    error::DecodeResult,
    traits::{Decoder, DecoderInput},
};

pub(crate) type BoxedSource = Box<dyn DecoderInput>;

/// Capability + factory protocol implemented by every decoder backend.
///
/// `Backend: Decoder` — every backend is also a [`Decoder`] at runtime,
/// so the dispatch site can box `B::try_create(...)` straight into
/// `Box<dyn Decoder>` without juggling associated types. Returning
/// `Self` from `try_create` requires `Self: Sized` implicitly, which
/// means `dyn Backend` is impossible — `Backend` is a static-dispatch-only
/// contract by design.
///
/// The `supports` method answers "will this backend decode this
/// codec/container?" without ever touching the source. When `container`
/// is `None`, the backend may either reject (hardware backends usually
/// need a known container) or accept and probe the container from raw
/// bytes (Symphonia).
pub(crate) trait Backend: Decoder {
    /// Whether this backend will decode `codec` (with `container` if given).
    fn supports(codec: AudioCodec, container: Option<ContainerFormat>) -> bool;

    /// Build a decoder of the concrete `Self` type. The backend is free
    /// to resolve a missing `container` from `codec` internally.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::UnsupportedCodec`] when the codec is not
    /// in the backend's accept list, plus any source / decoder
    /// initialisation errors.
    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> DecodeResult<Self>
    where
        Self: Sized;
}
