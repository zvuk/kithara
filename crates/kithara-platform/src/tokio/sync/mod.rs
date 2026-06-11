pub use tokio_with_wasm::alias::sync::*;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use crate::flash::tokio::sync::{mpsc, oneshot};
