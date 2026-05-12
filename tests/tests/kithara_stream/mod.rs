//! Integration tests for kithara-stream

#[cfg(not(target_arch = "wasm32"))]
mod reader_seek_overflow;
mod source;
mod source_position_contract;
mod sync_reader_basic_test;
