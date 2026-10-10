mod consts;
mod core;
mod dispatcher;

#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
mod ramped;

pub use core::{LaneAudio, load_audio, load_source_audio, wait_for_preload};
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub use core::{PcmDeck, open_resource};

pub use consts::{PRELOAD_READY_RETRIES, READ_PENDING_POLL};
pub use dispatcher::LaneLoader;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub use ramped::{InjectedFactory, RampedFactory};

#[cfg(test)]
mod tests;
