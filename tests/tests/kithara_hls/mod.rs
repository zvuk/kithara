//! Integration tests for kithara-hls

// NOTE: basestream modules disabled - they use old stream API that was removed
// pub mod basestream;
pub mod fixture;

mod abr_integration;
// mod basestream_abr;
// mod basestream_basic;
// mod basestream_seek;
mod basic_playback;
// mod cached_loader_edge_cases;  // uses old stream API
// mod cached_loader_integration;  // uses old open_v2 API
mod deferred_abr;
mod deferred_abr_debug;
// mod driver_test;  // uses old API
mod keys_integration;
mod playlist_integration;
mod seek_past_eof;
mod smoke_test;
mod source_seek;
mod stress_seek_abr;
mod sync_reader_hls_test;
