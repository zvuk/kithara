#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use crate::flash::sync::rwlock::*;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub use crate::native::sync::rwlock::*;
#[cfg(target_arch = "wasm32")]
pub use crate::wasm::sync::rwlock::*;
