#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]
#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
mod audio_artifact;
mod browser_runner_smoke;
#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
mod no_block;
