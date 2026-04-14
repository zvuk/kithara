#![no_main]

use arbitrary::Arbitrary;
use kithara_hls::internal::{
    parse_master_playlist, parse_media_playlist, variant_info_from_master,
};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    data: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let mut data = input.data;
    data.truncate(16 * 1024);

    if let Ok(master) = parse_master_playlist(&data) {
        let infos = variant_info_from_master(&master);
        assert_eq!(infos.len(), master.variants.len());
    }

    let _ = parse_media_playlist(&data);
});
