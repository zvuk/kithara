#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    data: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let s = String::from_utf8_lossy(&input.data);
    let _ = hls_m3u8::MasterPlaylist::try_from(s.as_ref());
    let _ = hls_m3u8::MediaPlaylist::try_from(s.as_ref());
});
