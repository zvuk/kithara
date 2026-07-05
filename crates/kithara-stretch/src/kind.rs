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
    /// shown nor clickable. Non-empty by construction: the crate requires at
    /// least one backend feature (`compile_error!` in `lib.rs` otherwise).
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            #[cfg(feature = "stretch-signalsmith")]
            Self::Signalsmith,
            #[cfg(feature = "stretch-bungee")]
            Self::Bungee,
        ]
    }
}

/// Stable discriminant for storing the selection in an atomic. Values are
/// fixed regardless of which feature-gated variants are compiled in.
impl From<StretchBackendKind> for u8 {
    fn from(kind: StretchBackendKind) -> Self {
        match kind {
            #[cfg(feature = "stretch-signalsmith")]
            StretchBackendKind::Signalsmith => 1,
            #[cfg(feature = "stretch-bungee")]
            StretchBackendKind::Bungee => 2,
        }
    }
}

/// Decode a stored backend discriminant. Any value outside the compiled-in set
/// decodes to the default (first compiled-in) backend.
impl From<u8> for StretchBackendKind {
    fn from(value: u8) -> Self {
        match value {
            #[cfg(feature = "stretch-signalsmith")]
            1 => Self::Signalsmith,
            #[cfg(feature = "stretch-bungee")]
            2 => Self::Bungee,
            _ => Self::all()[0],
        }
    }
}

/// The first compiled-in backend, in [`Self::all`] selector order.
impl Default for StretchBackendKind {
    fn default() -> Self {
        Self::all()[0]
    }
}

/// UI label = the variant name (`Signalsmith` / `Bungee`), via `Debug`, so
/// the selector needs no per-variant `cfg` arm.
impl fmt::Display for StretchBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
