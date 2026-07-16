mod facade;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

pub use facade::AudioPlayer;
