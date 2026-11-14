#![forbid(unsafe_code)]
#![recursion_limit = "256"]

use kithara_test_dylib as _;

mod progressive_continuity;
#[cfg(not(target_arch = "wasm32"))]
mod providers;
mod sync_fixture_census;
mod sync_listening;
#[cfg(not(target_arch = "wasm32"))]
mod sync_oracle;
#[cfg(not(target_arch = "wasm32"))]
mod sync_product_matrix;
mod sync_runtime_oracles;
mod sync_staging;
