#![forbid(unsafe_code)]
#![recursion_limit = "256"]

use kithara_test_dylib as _;

#[cfg(not(target_arch = "wasm32"))]
mod deck_membership;
#[cfg(not(target_arch = "wasm32"))]
mod metronome;
#[cfg(not(target_arch = "wasm32"))]
mod metronome_grid;
#[cfg(not(target_arch = "wasm32"))]
mod mix_tap;
mod mixing;
mod no_sync_real_media;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod offline_recording;
