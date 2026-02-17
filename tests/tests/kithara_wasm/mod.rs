#[cfg(not(target_arch = "wasm32"))]
mod ring_buffer_backpressure;

#[cfg(target_arch = "wasm32")]
mod stress;
