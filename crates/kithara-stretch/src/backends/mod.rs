#[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
mod bungee;
#[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
mod signalsmith;

#[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
pub(crate) use bungee::BungeeBackend;
#[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
pub(crate) use signalsmith::SignalsmithBackend;
