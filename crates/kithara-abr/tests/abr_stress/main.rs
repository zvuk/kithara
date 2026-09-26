#![forbid(unsafe_code)]

#[cfg(target_os = "android")]
use kithara_test_dylib as _;

mod abr_contract;
