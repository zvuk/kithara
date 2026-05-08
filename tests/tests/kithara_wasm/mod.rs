#[cfg(target_arch = "wasm32")]
mod stress;

#[cfg(all(not(target_arch = "wasm32"), feature = "selenium"))]
mod selenium;
