//! Single-type alias that resolves to the platform hardware backend at compile time.

#[cfg(not(any(
    all(feature = "apple", any(target_os = "macos", target_os = "ios")),
    all(feature = "android", target_os = "android")
)))]
use kithara_stream::{AudioCodec, ContainerFormat};

#[cfg(not(any(
    all(feature = "apple", any(target_os = "macos", target_os = "ios")),
    all(feature = "android", target_os = "android")
)))]
use super::{BoxedSource, HardwareBackend, RecoverableHardwareError};
#[cfg(not(any(
    all(feature = "apple", any(target_os = "macos", target_os = "ios")),
    all(feature = "android", target_os = "android")
)))]
use crate::{DecoderConfig, InnerDecoder};

/// Hardware backend selected at compile time.
///
/// - Apple `AudioToolbox` on macOS / iOS when the `apple` feature is enabled.
/// - Android `MediaCodec` on Android when the `android` feature is enabled.
/// - Otherwise [`NoopBackend`], which advertises no capabilities and is never
///   asked to construct a decoder — [`super::hardware_accepts`] short-circuits
///   first.
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
pub(crate) type Current = crate::apple::AppleBackend;

#[cfg(all(feature = "android", target_os = "android"))]
pub(crate) type Current = crate::android::AndroidBackend;

#[cfg(not(any(
    all(feature = "apple", any(target_os = "macos", target_os = "ios")),
    all(feature = "android", target_os = "android")
)))]
pub(crate) type Current = NoopBackend;

#[cfg(not(any(
    all(feature = "apple", any(target_os = "macos", target_os = "ios")),
    all(feature = "android", target_os = "android")
)))]
pub(crate) struct NoopBackend;

#[cfg(not(any(
    all(feature = "apple", any(target_os = "macos", target_os = "ios")),
    all(feature = "android", target_os = "android")
)))]
impl HardwareBackend for NoopBackend {
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
        _source: BoxedSource,
        _config: &DecoderConfig,
        _codec: AudioCodec,
        _container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        // `hardware_accepts::<Self>(..)` always returns `None`, so the
        // factory never reaches `try_create`.
        unreachable!("NoopBackend::try_create is gated by hardware_accepts")
    }
}
