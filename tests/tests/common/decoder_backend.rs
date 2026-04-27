//! Shared helper for e2e decoder-backend parameterisation.
//!
//! Each e2e test case runs once per decoder: Symphonia (software), Apple
//! (macOS/iOS hardware via `AudioToolbox`), Android (hardware via `MediaCodec`).
//! The helper maps `DecoderBackend` to the `prefer_hardware` flag on
//! `ResourceConfig`/`AudioConfig` and reports whether the backend is actually
//! compiled for the current target so cross-platform runs can skip gracefully.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecoderBackend {
    /// Symphonia software decoder — always available when the `symphonia`
    /// feature is on (the default). `prefer_hardware=false`.
    Symphonia,
    /// Apple hardware decoder via `AudioToolbox`. Only compiled on
    /// `target_os = "macos" | "ios"`. `prefer_hardware=true`.
    Apple,
    /// Android hardware decoder via `MediaCodec`. Only compiled on
    /// `target_os = "android"`. `prefer_hardware=true`.
    Android,
}

impl DecoderBackend {
    /// Maps directly to `ResourceConfig::prefer_hardware` / `AudioConfig::with_prefer_hardware`.
    pub(crate) const fn prefer_hardware(self) -> bool {
        !matches!(self, Self::Symphonia)
    }

    /// Is the backend compiled in on the current target? Callers use this to
    /// skip hardware cases on platforms where the backend cannot run.
    pub(crate) const fn is_available_on_current_target(self) -> bool {
        match self {
            Self::Symphonia => true,
            Self::Apple => cfg!(any(target_os = "macos", target_os = "ios")),
            Self::Android => cfg!(target_os = "android"),
        }
    }

    /// If unavailable, log a skip message and return `true` — callers should
    /// early-return to keep the test green on mismatched platforms.
    pub(crate) fn skip_if_unavailable(self) -> bool {
        if !self.is_available_on_current_target() {
            tracing::info!(
                backend = %self,
                "decoder backend unavailable on this target; skipping case"
            );
            return true;
        }
        false
    }
}

impl fmt::Display for DecoderBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symphonia => f.write_str("symphonia"),
            Self::Apple => f.write_str("apple"),
            Self::Android => f.write_str("android"),
        }
    }
}
