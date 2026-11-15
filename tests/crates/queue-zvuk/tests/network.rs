#![cfg(not(target_os = "android"))]
#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;
use kithara_test_dylib as _;

mod prod_network;
mod zvuk_prod_aac_to_flac_switch;
mod zvuk_prod_drm_e2e;
mod zvuk_prod_flac_swallow;
mod zvuk_stage_seed_brute_force;
