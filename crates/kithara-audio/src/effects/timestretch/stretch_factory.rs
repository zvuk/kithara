use kithara_decode::PcmSpec;

#[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
use super::bungee::BungeeBackend;
#[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
use super::signalsmith::SignalsmithBackend;
use super::stretch_backend::StretchBackend;
use super::stretch_kind::StretchBackendKind;
use super::timestretch_rs::TimestretchBackend;

/// Construct the backend for `kind` at the configured source `spec`. Called
/// once per chain build (and on a source-spec change inside the processor).
pub(crate) fn build_backend(kind: StretchBackendKind, spec: PcmSpec) -> Box<dyn StretchBackend> {
    match kind {
        StretchBackendKind::Timestretch => Box::new(TimestretchBackend::new(spec)),
        #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
        StretchBackendKind::Signalsmith => Box::new(SignalsmithBackend::new(spec)),
        #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
        StretchBackendKind::Bungee => Box::new(BungeeBackend::new(spec)),
    }
}
