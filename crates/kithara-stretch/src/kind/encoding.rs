use super::StretchKind;

/// Stable discriminant for storing the selection in an atomic. Values are
/// fixed regardless of which feature-gated variants are compiled in.
impl From<StretchKind> for u8 {
    fn from(kind: StretchKind) -> Self {
        match kind {
            #[cfg(feature = "stretch-signalsmith")]
            StretchKind::Signalsmith => 1,
            #[cfg(feature = "stretch-bungee")]
            StretchKind::Bungee => 2,
            #[cfg(feature = "stretch-glide")]
            StretchKind::Glide => 3,
        }
    }
}

/// Decode a stored backend discriminant. Any value outside the compiled-in set
/// decodes to the default (first compiled-in) backend.
impl From<u8> for StretchKind {
    fn from(value: u8) -> Self {
        match value {
            #[cfg(feature = "stretch-signalsmith")]
            1 => Self::Signalsmith,
            #[cfg(feature = "stretch-bungee")]
            2 => Self::Bungee,
            #[cfg(feature = "stretch-glide")]
            3 => Self::Glide,
            _ => Self::all()[0],
        }
    }
}
