#![no_main]

use arbitrary::Arbitrary;
use kithara_play::ResourceConfig;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    raw: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let mut raw = input.raw;
    raw.truncate(4 * 1024);

    let text = String::from_utf8_lossy(&raw);
    let _ = ResourceConfig::new(text.as_ref());
});
