#![forbid(unsafe_code)]

use kithara_test_dylib as _;

#[path = "alloc_free_hotpath.rs"]
mod alloc_free_hotpath;
