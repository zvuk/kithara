#![forbid(unsafe_code)]

#[cfg(target_os = "android")]
use kithara_test_dylib as _;

mod aac_tests;
mod bytes_tests;
mod flac_tests;
mod stream_tests;
