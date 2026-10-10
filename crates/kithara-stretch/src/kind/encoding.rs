use super::StretchKind;

/// Stable discriminant for storing the selection in an atomic. Values are
/// fixed regardless of which feature-gated variants are compiled in.
impl From<StretchKind> for u8 {
    fn from(kind: StretchKind) -> Self {
        match kind {
            #[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
            StretchKind::Signalsmith => 1,
            #[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
            StretchKind::Bungee => 2,
            #[cfg(feature = "stretch-glide")]
            StretchKind::Glide => 3,
            #[cfg(feature = "stretch-identity")]
            StretchKind::Identity => 4,
        }
    }
}

/// Decode a stored backend discriminant. Any value outside the compiled-in set
/// decodes to the default (first compiled-in) backend.
impl From<u8> for StretchKind {
    fn from(value: u8) -> Self {
        match value {
            #[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
            1 => Self::Signalsmith,
            #[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
            2 => Self::Bungee,
            #[cfg(feature = "stretch-glide")]
            3 => Self::Glide,
            #[cfg(feature = "stretch-identity")]
            4 => Self::Identity,
            _ => Self::all()[0],
        }
    }
}
