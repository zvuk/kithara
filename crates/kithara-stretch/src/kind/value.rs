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
/// feature). Selecting an absent backend is
/// un-representable rather than a runtime error.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq, serde::Deserialize)]
#[display("{self:?}")]
pub enum StretchKind {
    /// `signalsmith-stretch` (C++). Feature `stretch-signalsmith`.
    #[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
    Signalsmith,
    /// `bungee` (C++). Feature `stretch-bungee`.
    #[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
    Bungee,
    /// Pure-Rust Glide resampler. Changes pitch with playback rate.
    #[cfg(feature = "stretch-glide")]
    Glide,
    /// Unity-rate sample copy without pitch or rate changes.
    #[cfg(feature = "stretch-identity")]
    Identity,
}
