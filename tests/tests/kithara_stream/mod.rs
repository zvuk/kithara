//! Integration tests for kithara-stream

mod architecture_progressive_download;
#[cfg(not(target_arch = "wasm32"))]
mod reader_seek_overflow;
mod source;
mod sync_reader_basic_test;
#[cfg(not(target_arch = "wasm32"))]
mod timeline_source_of_truth;
#[cfg(not(target_arch = "wasm32"))]
mod writer_no_auto_commit;
#[cfg(not(target_arch = "wasm32"))]
mod writer_with_offset;
