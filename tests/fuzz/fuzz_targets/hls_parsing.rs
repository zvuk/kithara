#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try parsing arbitrary bytes as M3U8 playlists.
    // hls_m3u8 should never panic on malformed input.
    // Use lossy conversion to exercise the parser with invalid UTF-8 too.
    let s = String::from_utf8_lossy(data);
    let _ = hls_m3u8::MasterPlaylist::try_from(s.as_ref());
    let _ = hls_m3u8::MediaPlaylist::try_from(s.as_ref());
});
