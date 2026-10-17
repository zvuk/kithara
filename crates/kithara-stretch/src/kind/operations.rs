use super::{BackendCapabilities, StretchKind};

impl StretchKind {
    /// Backends compiled into this target/feature set, in selector order.
    /// Non-empty by construction: the crate requires a backend feature.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            #[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
            Self::Signalsmith,
            #[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
            Self::Bungee,
            #[cfg(feature = "stretch-glide")]
            Self::Glide,
            #[cfg(feature = "stretch-identity")]
            Self::Identity,
        ]
    }

    /// Functions supported by this backend.
    #[must_use]
    pub const fn capabilities(self) -> BackendCapabilities {
        match self {
            #[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
            Self::Signalsmith => BackendCapabilities::RATE.union(BackendCapabilities::KEYLOCK),
            #[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
            Self::Bungee => BackendCapabilities::RATE.union(BackendCapabilities::KEYLOCK),
            #[cfg(feature = "stretch-glide")]
            Self::Glide => BackendCapabilities::RATE,
            #[cfg(feature = "stretch-identity")]
            Self::Identity => BackendCapabilities::empty(),
        }
    }
}

/// The first compiled-in backend, in [`StretchKind::all`] selector order.
impl Default for StretchKind {
    fn default() -> Self {
        Self::all()[0]
    }
}
