#![forbid(unsafe_code)]

use kithara_test_dylib as _;

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}

mod abr_auto_switch;
mod abr_mode_switch;
mod abr_playback;
mod abr_switch_playback;
mod stress_seek_abr;
