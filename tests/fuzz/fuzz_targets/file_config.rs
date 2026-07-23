#![no_main]

use std::{path::PathBuf, sync::LazyLock};

use arbitrary::{Arbitrary, Unstructured};
use kithara::{
    assets::{AssetStore, AssetStoreBuilder, StorageBackend},
    file::{FileConfig, FileSrc},
};
use libfuzzer_sys::fuzz_target;
use url::Url;

static STORE: LazyLock<AssetStore> = LazyLock::new(|| {
    AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build()
});

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
    let cfg = FileConfig::for_src(src)
        .store(STORE.clone())
        .discriminator(name.as_ref().to_string())
        .build();

    if let Some(stored) = cfg.discriminator.as_ref() {
        assert!(stored.len() <= name.len());
    }
});
