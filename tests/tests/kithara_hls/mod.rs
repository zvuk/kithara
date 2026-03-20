//! Integration tests for kithara-hls

pub(crate) mod fixture;

mod abr_switch_playback;
mod abr_integration;
mod basic_playback;
mod deferred_abr;
mod deferred_abr_debug;
mod drm_stream_integrity;
mod driver_test;
mod ephemeral;
mod keys_integration;
mod live_stress_real_stream;
mod playlist_integration;
mod seek_past_eof;
mod seek_variant_switch_after_eof;
mod smoke_test;
mod source_seek;
mod stress_chunk_integrity;
mod stress_seek_abr;
mod stress_seek_abr_audio;
mod stress_seek_audio;
mod stress_seek_lifecycle;
mod stress_seek_random;
mod sync_reader_hls_test;
mod wait_range_contract;
