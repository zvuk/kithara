#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let _ = hls_m3u8::MasterPlaylist::try_from(s.as_ref());
    let _ = hls_m3u8::MediaPlaylist::try_from(s.as_ref());
});
