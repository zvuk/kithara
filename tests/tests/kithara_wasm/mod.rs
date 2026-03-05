#[cfg(target_arch = "wasm32")]
mod stress;

#[cfg(not(target_arch = "wasm32"))]
mod selenium;
