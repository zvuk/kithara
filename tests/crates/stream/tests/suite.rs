#![forbid(unsafe_code)]

//! Integration tests for kithara-stream

use kithara_test_dylib as _;

#[path = "../../../src/memory_source.rs"]
mod memory_source;
mod source;
