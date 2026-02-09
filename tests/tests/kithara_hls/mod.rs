//! Integration tests for kithara-hls

pub mod fixture;

mod abr_integration;
mod basic_playback;
mod deferred_abr;
mod deferred_abr_debug;
mod driver_test;
mod keys_integration;
mod playlist_integration;
mod seek_past_eof;
mod smoke_test;
mod source_seek;
mod stress_seek_abr;
mod stress_seek_abr_audio;
mod stress_seek_audio;
mod stress_seek_random;
mod sync_reader_hls_test;
