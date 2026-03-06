#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try parsing arbitrary bytes as M3U8 playlists.
    // hls_m3u8 should never panic on malformed input.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = hls_m3u8::MasterPlaylist::try_from(s);
        let _ = hls_m3u8::MediaPlaylist::try_from(s);
    }
});
