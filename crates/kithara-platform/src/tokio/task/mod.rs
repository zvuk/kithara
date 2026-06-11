#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use crate::flash::tokio::task::*;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub use crate::native::tokio::task::*;
#[cfg(target_arch = "wasm32")]
pub use crate::wasm::tokio::task::*;
