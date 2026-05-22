//! Cross-platform FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API and lets `UniFFI`
//! generate platform bindings such as Swift and Kotlin.

#[cfg(feature = "backend-uniffi")]
uniffi::setup_scaffolding!();

#[cfg(feature = "backend-uniffi")]
use kithara_events::TrackId;

// Expose `kithara_events::TrackId` (a `u64` newtype) to UniFFI as a
// transparent `u64`. Keeps `audioId: TrackId` strongly typed on the
// Swift / Kotlin side instead of leaking a raw integer; conversion
// uses the `From<u64>` / `From<TrackId>` impls in `kithara-events`.
// `remote` bypasses Rust's orphan rule — `TrackId` lives in a
// different crate.
#[cfg(feature = "backend-uniffi")]
uniffi::custom_type!(TrackId, u64, { remote });

#[cfg(target_os = "android")]
pub(crate) mod android;
#[cfg(all(target_os = "android", feature = "test"))]
pub(crate) mod android_test;
pub mod cipher;
pub mod config;
pub(crate) mod event_bridge;
pub mod item;
pub(crate) mod item_bridge;
pub mod logging;
pub mod observer;
pub mod player;
mod runtime;
pub mod types;

pub(crate) use runtime::FFI_RUNTIME;
