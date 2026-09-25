mod eq;
mod facade;
mod policy;
mod selection;
mod session;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use eq::validate_eq_band_count;
pub use facade::AudioPlayer;
