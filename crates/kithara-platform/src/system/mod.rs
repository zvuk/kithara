//! Pure native backend. Knows nothing about the other backends;
//! cross-platform code lives in `crate::common` and is re-imported here.
//! Compiled only off wasm32 (gated in `lib.rs`), so the tree itself
//! carries no cfg.

pub mod env;
#[cfg(not(feature = "loom"))]
#[path = "support/errors.rs"]
pub(crate) mod errors;
#[path = "support/logging.rs"]
pub mod logging;
#[path = "support/maybe_send.rs"]
pub mod maybe_send;
pub(crate) mod ownership;
#[cfg(feature = "loom")]
#[path = "support/poison.rs"]
pub(crate) mod poison;
pub mod time;
pub mod tokio;
