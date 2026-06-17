use std::fmt;

/// Stretch backend selection. Variants exist only when their backend is
/// compiled in (this module itself requires at least one `stretch-*`
/// feature on a native target). Selecting an absent backend is
/// un-representable rather than a runtime error.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StretchBackendKind {
    /// `signalsmith-stretch` (C++). Feature `stretch-signalsmith`.
    #[cfg(feature = "stretch-signalsmith")]
    Signalsmith,
    /// `bungee` (C++). Feature `stretch-bungee`.
    #[cfg(feature = "stretch-bungee")]
    Bungee,
}

impl StretchBackendKind {
    /// Backends compiled into this target/feature set, in selector order.
    /// The DJ UI renders exactly these, so an unavailable backend is never
    /// shown nor clickable. Non-empty by construction: this module only
    /// compiles when at least one `stretch-*` feature is enabled.
    pub const ALL: &'static [Self] = &[
        #[cfg(feature = "stretch-signalsmith")]
        Self::Signalsmith,
        #[cfg(feature = "stretch-bungee")]
        Self::Bungee,
    ];

    /// Decode a discriminant written by [`Self::to_u8`]. Any value outside the
    /// compiled-in set decodes to the default (first compiled-in) backend.
    pub(crate) const fn from_u8(v: u8) -> Self {
        match v {
            #[cfg(feature = "stretch-signalsmith")]
            1 => Self::Signalsmith,
            #[cfg(feature = "stretch-bungee")]
            2 => Self::Bungee,
            _ => Self::ALL[0],
        }
    }

    /// Stable discriminant for storing the selection in an atomic. Values are
    /// fixed regardless of which feature-gated variants are compiled in.
    pub(crate) const fn to_u8(self) -> u8 {
        match self {
            #[cfg(feature = "stretch-signalsmith")]
            Self::Signalsmith => 1,
            #[cfg(feature = "stretch-bungee")]
            Self::Bungee => 2,
        }
    }
}

/// The first compiled-in backend, in [`Self::ALL`] selector order.
impl Default for StretchBackendKind {
    fn default() -> Self {
        Self::ALL[0]
    }
}

/// UI label = the variant name (`Signalsmith` / `Bungee`), via `Debug`, so
/// the selector needs no per-variant `cfg` arm.
impl fmt::Display for StretchBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
