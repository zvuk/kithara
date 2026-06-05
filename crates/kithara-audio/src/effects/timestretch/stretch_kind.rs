use std::fmt;

/// Stretch backend selection. Variants exist only when their backend is
/// compiled in: the pure-Rust `Timestretch` is always present, the C++
/// backends are native-only and feature-gated. Selecting an absent
/// backend is un-representable rather than a runtime error.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StretchBackendKind {
    /// Pure-Rust `timestretch`. Always available; the only wasm backend.
    #[default]
    Timestretch,
    /// `signalsmith-stretch` (C++). Native-only, feature `stretch-signalsmith`.
    #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
    Signalsmith,
    /// `bungee` (C++). Native-only, feature `stretch-bungee`.
    #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
    Bungee,
}

impl StretchBackendKind {
    /// Backends compiled into this target/feature set, in selector order.
    /// The DJ UI renders exactly these, so an unavailable backend is never
    /// shown nor clickable.
    pub const ALL: &'static [Self] = &[
        Self::Timestretch,
        #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
        Self::Signalsmith,
        #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
        Self::Bungee,
    ];

    /// Stable discriminant for storing the selection in an atomic. Values are
    /// fixed regardless of which feature-gated variants are compiled in.
    pub(crate) const fn to_u8(self) -> u8 {
        match self {
            Self::Timestretch => 0,
            #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
            Self::Signalsmith => 1,
            #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
            Self::Bungee => 2,
        }
    }

    /// Decode a discriminant written by [`Self::to_u8`]. Any value outside the
    /// compiled-in set decodes to the always-available `Timestretch` (covers
    /// `0` and the unreachable case of a variant that is not compiled here).
    pub(crate) const fn from_u8(v: u8) -> Self {
        match v {
            #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
            1 => Self::Signalsmith,
            #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
            2 => Self::Bungee,
            _ => Self::Timestretch,
        }
    }
}

/// UI label = the variant name (`Timestretch` / `Signalsmith` / `Bungee`),
/// via `Debug`, so the selector needs no per-variant `cfg` arm.
impl fmt::Display for StretchBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
