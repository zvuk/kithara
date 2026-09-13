#![forbid(unsafe_code)]

//! Integration tests for kithara-stream

#[path = "../../../src/memory_source.rs"]
mod memory_source;
#[cfg(not(target_arch = "wasm32"))]
mod reader_seek_overflow;
mod source;
mod sync_reader_basic_test;
