#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;

#[path = "source_helper.rs"]
mod source_helper;
pub(crate) use source_helper::{app_disk_asset_store, app_track_source};

#[path = "false_eof_rapid_scrub.rs"]
mod false_eof_rapid_scrub;
#[path = "real_playlist.rs"]
mod real_playlist;
#[path = "zvuk_prod_aac_to_flac_switch.rs"]
mod zvuk_prod_aac_to_flac_switch;
#[path = "zvuk_prod_drm_e2e.rs"]
mod zvuk_prod_drm_e2e;
#[path = "zvuk_prod_flac_swallow.rs"]
mod zvuk_prod_flac_swallow;
#[path = "zvuk_stage_seed_brute_force.rs"]
mod zvuk_stage_seed_brute_force;

#[path = "user_simulation/prod_network.rs"]
mod prod_network;
