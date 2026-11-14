#![cfg(test)]
#![forbid(unsafe_code)]

use kithara_test_dylib as _;

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}

mod cancel_isolation;
mod config_with_downloader;
mod ephemeral;
mod html_error_cleanup;
mod red_leak_pattern;
mod red_leak_peer_handle_cycle;
mod red_leak_small_cache_seek;
mod red_stale_tmp_claim_bricks_segment;
