use super::{BackendCapabilities, StretchKind};

impl StretchKind {
    /// Backends compiled into this target/feature set, in selector order.
    /// Non-empty by construction: the crate requires a backend feature.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            #[cfg(feature = "stretch-signalsmith")]
            Self::Signalsmith,
            #[cfg(feature = "stretch-bungee")]
            Self::Bungee,
            #[cfg(feature = "stretch-glide")]
            Self::Glide,
        ]
    }

    /// Functions supported by this backend.
    #[must_use]
    pub const fn capabilities(self) -> BackendCapabilities {
        match self {
            #[cfg(feature = "stretch-signalsmith")]
            Self::Signalsmith => BackendCapabilities::RATE.union(BackendCapabilities::KEYLOCK),
            #[cfg(feature = "stretch-bungee")]
            Self::Bungee => BackendCapabilities::RATE.union(BackendCapabilities::KEYLOCK),
            #[cfg(feature = "stretch-glide")]
            Self::Glide => BackendCapabilities::RATE,
        }
    }
}

/// The first compiled-in backend, in [`StretchKind::all`] selector order.
impl Default for StretchKind {
    fn default() -> Self {
        Self::all()[0]
    }
}
