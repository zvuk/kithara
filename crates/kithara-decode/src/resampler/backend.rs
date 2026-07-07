#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResamplerBackend {
    #[cfg(feature = "resample-rubato")]
    Rubato,
    #[cfg(feature = "resample-fft")]
    Fft,
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    Apple,
}

impl ResamplerBackend {
    /// Backends compiled into this target/feature set, in selector order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            #[cfg(feature = "resample-rubato")]
            Self::Rubato,
            #[cfg(feature = "resample-fft")]
            Self::Fft,
            #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
            Self::Apple,
        ]
    }

    /// Backends eligible for playback-stage automatic selection.
    #[must_use]
    pub const fn playback() -> &'static [Self] {
        &[
            #[cfg(feature = "resample-rubato")]
            Self::Rubato,
            #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
            Self::Apple,
        ]
    }

    /// Preferred backend for this target/feature set.
    #[must_use]
    pub fn preferred() -> Option<Self> {
        Self::playback().first().copied()
    }
}
