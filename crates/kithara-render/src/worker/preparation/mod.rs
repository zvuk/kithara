//! Cold preparation has a separate native owner; browser sources retain executor affinity.

#[cfg(not(target_arch = "wasm32"))]
#[path = "native.rs"]
mod implementation;
#[cfg(target_arch = "wasm32")]
#[path = "browser.rs"]
mod implementation;

pub(super) use implementation::{Preparation, execute, new};

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "tests.rs"]
mod tests;
