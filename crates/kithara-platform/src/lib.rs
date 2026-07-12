//! Platform-aware primitives with one compile-time-selected backend.
//! Backends mirror the public sync, thread, time, and Tokio facade.

mod common;

#[cfg(all(
    not(target_arch = "wasm32"),
    not(feature = "flash"),
    not(feature = "loom")
))]
#[path = "backend/system.rs"]
mod backend;
#[cfg(all(not(target_arch = "wasm32"), feature = "loom", not(feature = "flash")))]
#[path = "backend/loom.rs"]
mod backend;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash", not(feature = "loom")))]
#[path = "backend/flash_system.rs"]
mod backend;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash", feature = "loom"))]
#[path = "backend/flash_loom.rs"]
mod backend;
#[cfg(all(not(target_arch = "wasm32"), feature = "loom"))]
mod loom;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash", feature = "loom"))]
#[path = "system/flash_loom.rs"]
mod system;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash", not(feature = "loom")))]
#[path = "system/flash_system.rs"]
mod system;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
mod system;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub use backend::{env, logging, maybe_send, sync, thread, time, tokio};

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod __private;

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub mod flash;
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[path = "common/flash_inert.rs"]
pub mod flash;
#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
pub mod no_block;
#[cfg(not(all(not(target_arch = "wasm32"), feature = "no-block")))]
#[path = "common/no_block_inert.rs"]
pub mod no_block;

pub use common::{
    cancel::{CancelGroup, CancelScope, CancelToken, CancelWakerGuard, Cancelled},
    traits,
};
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use flash::*;
