#[cfg(feature = "stretch-bungee")]
use crate::backends::BungeeBackend;
#[cfg(feature = "stretch-signalsmith")]
use crate::backends::SignalsmithBackend;
use crate::{StretchBackend, StretchBackendKind, StretchOptions};

/// Construct the backend for `kind` at the configured source shape. Called once
/// per chain build and on a source-spec change inside the audio processor.
#[must_use]
pub fn build_backend(
    kind: StretchBackendKind,
    options: &StretchOptions,
) -> Box<dyn StretchBackend> {
    match kind {
        #[cfg(feature = "stretch-signalsmith")]
        StretchBackendKind::Signalsmith => Box::new(SignalsmithBackend::new(options)),
        #[cfg(feature = "stretch-bungee")]
        StretchBackendKind::Bungee => Box::new(BungeeBackend::new(options)),
    }
}
