#![forbid(unsafe_code)]

use kithara_test_dylib as _;

mod abr_integration;
mod deferred_abr;
mod red_abr_no_escape_from_stalled_variant;
mod seek_variant_switch_after_eof;
