#![no_main]

use arbitrary::Arbitrary;
use kithara::{
    events::VariantInfo,
    hls::{FromWithParams, parse_master_playlist, parse_media_playlist},
};
use libfuzzer_sys::fuzz_target;
use url::Url;

#[derive(Arbitrary, Debug)]
struct Input {
    data: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let mut data = input.data;
    data.truncate(16 * 1024);

    if let Ok(master) = parse_master_playlist(&data) {
        let infos: Vec<VariantInfo> = FromWithParams::build(&master, &[][..]);
        assert_eq!(infos.len(), master.variants.len());
    }

    let url: Url = "http://fuzz.invalid/playlist.m3u8"
        .parse()
        .expect("static url");
    let _ = parse_media_playlist(url, &data);
});
