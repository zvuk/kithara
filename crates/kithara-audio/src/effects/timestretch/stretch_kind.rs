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
}

/// UI label = the variant name (`Timestretch` / `Signalsmith` / `Bungee`),
/// via `Debug`, so the selector needs no per-variant `cfg` arm.
impl fmt::Display for StretchBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
