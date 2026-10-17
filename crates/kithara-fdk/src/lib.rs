#[cfg(not(target_arch = "wasm32"))]
mod decoder;

#[cfg(not(target_arch = "wasm32"))]
pub use decoder::{Decoder, Error};
#[cfg(not(target_arch = "wasm32"))]
pub use fdk_aac_sys::CStreamInfo as StreamInfo;
