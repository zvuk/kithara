#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]
#![forbid(unsafe_code)]

use kithara_test_dylib as _;

#[cfg(not(target_arch = "wasm32"))]
mod audio_artifact;
mod browser_runner_smoke;
mod cochlea_continuity_oracle;
#[cfg(not(target_arch = "wasm32"))]
mod fixture_server;
#[cfg(all(not(target_arch = "wasm32"), feature = "no-block"))]
mod no_block;
mod offline_harness_smoke;
