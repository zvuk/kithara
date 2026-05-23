//! Cross-platform FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API. Native targets
//! (Apple / Android) use `UniFFI` to generate Swift / Kotlin bindings; wasm32
//! uses wasm-bindgen under [`crate::web`]. `src/lib.rs` is the single
//! structural boundary where target-conditional `cfg` gates live (one
//! `target_os = "android"` gate + the `target_arch = "wasm32"` split).

#[cfg(all(feature = "backend-uniffi", not(target_arch = "wasm32")))]
uniffi::setup_scaffolding!();

#[cfg(all(feature = "backend-uniffi", not(target_arch = "wasm32")))]
use kithara_events::TrackId;

// Expose `kithara_events::TrackId` (a `u64` newtype) to UniFFI as a
// transparent `u64`. Keeps `audioId: TrackId` strongly typed on the
// Swift / Kotlin side instead of leaking a raw integer; conversion
// uses the `From<u64>` / `From<TrackId>` impls in `kithara-events`.
// `remote` bypasses Rust's orphan rule — `TrackId` lives in a
// different crate.
#[cfg(all(feature = "backend-uniffi", not(target_arch = "wasm32")))]
uniffi::custom_type!(TrackId, u64, { remote });

#[cfg(target_os = "android")]
pub(crate) mod android;
#[cfg(all(target_os = "android", feature = "test"))]
pub(crate) mod android_test;
#[cfg(not(target_arch = "wasm32"))]
pub mod cipher;
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod event_bridge;
#[cfg(not(target_arch = "wasm32"))]
pub mod item;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod item_bridge;
#[cfg(not(target_arch = "wasm32"))]
pub mod logging;
#[cfg(not(target_arch = "wasm32"))]
pub mod observer;
#[cfg(not(target_arch = "wasm32"))]
pub mod player;
#[cfg(not(target_arch = "wasm32"))]
mod runtime;
#[cfg(not(target_arch = "wasm32"))]
pub mod types;

#[cfg(target_arch = "wasm32")]
pub mod web;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use runtime::FFI_RUNTIME;
