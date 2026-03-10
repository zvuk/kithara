#![no_main]

use std::path::PathBuf;

use arbitrary::{Arbitrary, Unstructured};
use kithara_file::{FileConfig, FileSrc};
use libfuzzer_sys::fuzz_target;
use url::Url;

#[derive(Debug)]
struct Input {
    name: Vec<u8>,
    raw: Vec<u8>,
    remote: bool,
}

impl<'a> Arbitrary<'a> for Input {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let mut name = u.arbitrary::<Vec<u8>>()?;
        name.truncate(128);
        let mut raw = u.arbitrary::<Vec<u8>>()?;
        raw.truncate(512);
        Ok(Self {
            name,
            raw,
            remote: u.arbitrary()?,
        })
    }
}

fuzz_target!(|input: Input| {
    let raw = String::from_utf8_lossy(&input.raw);
    let src = if input.remote {
        match Url::parse(raw.as_ref()) {
            Ok(url) => FileSrc::Remote(url),
            Err(_) => FileSrc::Local(PathBuf::from("/tmp/fuzz_audio.bin")),
        }
    } else {
        FileSrc::Local(PathBuf::from(raw.as_ref()))
    };

    let name = String::from_utf8_lossy(&input.name);
    let cfg = FileConfig::new(src).with_name(name.as_ref());

    if let Some(stored) = cfg.name.as_ref() {
        assert!(stored.len() <= name.len());
    }
});
