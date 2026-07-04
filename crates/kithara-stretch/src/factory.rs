#[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
use crate::backends::BungeeBackend;
#[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
use crate::backends::SignalsmithBackend;
use crate::{StretchBackend, StretchBackendKind};

/// Construct the backend for `kind` at the configured source shape. Called once
/// per chain build and on a source-spec change inside the audio processor.
#[must_use]
pub fn build_backend(
    kind: StretchBackendKind,
    sample_rate: u32,
    channels: usize,
) -> Box<dyn StretchBackend> {
    match kind {
        #[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
        StretchBackendKind::Signalsmith => Box::new(SignalsmithBackend::new(sample_rate, channels)),
        #[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
        StretchBackendKind::Bungee => Box::new(BungeeBackend::new(sample_rate, channels)),
    }
}
