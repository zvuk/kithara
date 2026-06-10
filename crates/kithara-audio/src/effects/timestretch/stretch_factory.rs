use kithara_decode::PcmSpec;

#[cfg(feature = "stretch-bungee")]
use super::bungee::BungeeBackend;
#[cfg(feature = "stretch-signalsmith")]
use super::signalsmith::SignalsmithBackend;
use super::stretch_backend::StretchBackend;
use super::stretch_kind::StretchBackendKind;

/// Construct the backend for `kind` at the configured source `spec`. Called
/// once per chain build (and on a source-spec change inside the processor).
pub(crate) fn build_backend(kind: StretchBackendKind, spec: PcmSpec) -> Box<dyn StretchBackend> {
    match kind {
        #[cfg(feature = "stretch-signalsmith")]
        StretchBackendKind::Signalsmith => Box::new(SignalsmithBackend::new(spec)),
        #[cfg(feature = "stretch-bungee")]
        StretchBackendKind::Bungee => Box::new(BungeeBackend::new(spec)),
    }
}
