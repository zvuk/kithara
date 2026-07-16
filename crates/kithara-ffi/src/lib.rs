//! Cross-platform FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API. Native targets
//! (Apple / Android) use `UniFFI` to generate Swift / Kotlin bindings; wasm32
//! uses wasm-bindgen under `web`. `src/lib.rs` is the single
//! structural boundary where target-conditional `cfg` gates live: shared
//! FFI data types live in `core`, native-only bridges/runtime in `native`,
//! and the wasm surface in `web`.

#[cfg(all(feature = "uniffi", not(target_arch = "wasm32")))]
uniffi::setup_scaffolding!();

#[cfg(all(feature = "uniffi", not(target_arch = "wasm32")))]
use kithara_events::TrackId;

#[cfg(all(feature = "uniffi", not(target_arch = "wasm32")))]
uniffi::custom_type!(TrackId, u64, { remote });

mod core;
#[cfg(not(target_arch = "wasm32"))]
mod native;
pub mod player;
#[cfg(target_arch = "wasm32")]
pub mod web;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use core::registry;
pub use core::{item, layout, observer, types};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::{FFI_RUNTIME, Inner, event_bridge};
#[cfg(not(target_arch = "wasm32"))]
pub use native::{asset, cipher, config, logging, salt};
#[cfg(target_arch = "wasm32")]
pub(crate) use web::inner::Inner;
