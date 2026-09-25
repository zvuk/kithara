bitflags::bitflags! {
    /// Functions a compiled stretch backend can actually render.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct BackendCapabilities: u8 {
        /// Change source advance per output frame.
        const RATE = 0b01;
        /// Change source advance while preserving pitch.
        const KEYLOCK = 0b10;
    }
}

/// Stretch backend selection. Variants exist only when their backend is
/// compiled in (this module itself requires at least one `stretch-*`
/// feature on a native target). Selecting an absent backend is
/// un-representable rather than a runtime error.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[display("{self:?}")]
pub enum StretchKind {
    /// `signalsmith-stretch` (C++). Feature `stretch-signalsmith`.
    #[cfg(feature = "stretch-signalsmith")]
    Signalsmith,
    /// `bungee` (C++). Feature `stretch-bungee`.
    #[cfg(feature = "stretch-bungee")]
    Bungee,
    /// Pure-Rust Glide resampler. Changes pitch with playback rate.
    #[cfg(feature = "stretch-glide")]
    Glide,
}

/// UI label = the variant name (`Signalsmith` / `Bungee`), via `Debug`, so
/// the selector needs no per-variant `cfg` arm.
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
