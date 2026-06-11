#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use crate::flash::time::*;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub use crate::native::time::*;
#[cfg(target_arch = "wasm32")]
pub use crate::wasm::time::*;
