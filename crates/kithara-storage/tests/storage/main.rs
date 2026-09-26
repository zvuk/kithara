#![forbid(unsafe_code)]

//! Integration tests for kithara-storage

#[cfg(target_os = "android")]
use kithara_test_dylib as _;

mod atomic;
mod streaming;
mod support;
