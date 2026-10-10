#[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
mod bungee;
#[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
mod signalsmith;

#[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
pub(crate) use bungee::BungeeElastic;
#[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
pub(crate) use signalsmith::SignalsmithElastic;

#[cfg(feature = "stretch-identity")]
mod identity;
#[cfg(feature = "stretch-identity")]
pub(crate) use identity::IdentityElastic;

#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
mod varispeed;
#[cfg(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
))]
pub(crate) use varispeed::VarispeedElastic;
