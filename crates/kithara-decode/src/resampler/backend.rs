#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResamplerBackend {
    #[cfg(feature = "resample-rubato")]
    Rubato,
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    Apple,
}

impl Default for ResamplerBackend {
    fn default() -> Self {
        Self::all()[0]
    }
}

impl ResamplerBackend {
    /// Backends compiled into this target/feature set, in selector order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            #[cfg(feature = "resample-rubato")]
            Self::Rubato,
            #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
            Self::Apple,
        ]
    }
}
