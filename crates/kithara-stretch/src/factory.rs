#[cfg(feature = "stretch-bungee")]
use crate::backends::BungeeBackend;
#[cfg(feature = "stretch-signalsmith")]
use crate::backends::SignalsmithBackend;
use crate::{StretchBackend, StretchBackendError, StretchKind, StretchOptions};

/// Construct the backend for `kind` at the configured source shape. Called once
/// per chain build and on a source-spec change inside the audio processor.
///
/// # Errors
///
/// Returns [`StretchBackendError::Construction`] when the selected backend
/// cannot initialize its fixed-capacity processing state.
pub fn build_backend(
    kind: StretchKind,
    options: &StretchOptions,
) -> Result<Box<dyn StretchBackend>, StretchBackendError> {
    Ok(match kind {
        #[cfg(feature = "stretch-signalsmith")]
        StretchKind::Signalsmith => Box::new(SignalsmithBackend::new(options)),
        #[cfg(feature = "stretch-bungee")]
        StretchKind::Bungee => Box::new(BungeeBackend::new(options)?),
    })
}

#[cfg(test)]
#[path = "factory_tests.rs"]
mod tests;
