//! Cross-platform FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API. Native targets
//! (Apple / Android) use `UniFFI` to generate Swift / Kotlin bindings; wasm32
//! uses the generated `UniFFI` configuration surface alongside the existing
//! wasm-bindgen player adapter under `web`. `src/lib.rs` is the single
//! structural boundary where target-conditional `cfg` gates live: shared
//! FFI data types live in `core`, native-only bridges/runtime in `native`,
//! and the wasm surface in `web`.

#[cfg(any(feature = "uniffi", feature = "uniffi-web"))]
uniffi::setup_scaffolding!();

#[cfg(any(feature = "uniffi", feature = "uniffi-web"))]
use kithara::events::TrackId;

#[cfg(any(feature = "uniffi", feature = "uniffi-web"))]
uniffi::custom_type!(TrackId, u64, { remote });

mod core;
#[cfg(not(target_arch = "wasm32"))]
mod native;
pub mod player;
pub mod pools;
#[cfg(target_arch = "wasm32")]
pub mod web;

#[cfg(not(target_arch = "wasm32"))]
pub use core::host::ensure_default_host;
#[cfg(all(target_arch = "wasm32", feature = "uniffi-web"))]
pub use core::host::tick_host;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use core::registry;
pub use core::{
    FfiEqBandConfig, FfiEqFilterKind, FfiFileSourceSettings, FfiHlsSourceSettings, FfiHostConfig,
    FfiLimiterConfig, FfiQueueSettings, FfiSizeProbeMethod, FfiSourceSettings, analysis,
    default_host_config, host::initialize_host, item, layout, observer, types,
};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::{FFI_RUNTIME, Inner, event_bridge};
#[cfg(not(target_arch = "wasm32"))]
pub use native::{asset, cipher, config, logging, salt};
#[cfg(target_arch = "wasm32")]
pub(crate) use web::inner::Inner;
