#![no_main]

use std::sync::LazyLock;

use arbitrary::Arbitrary;
use kithara::{
    assets::{AssetStore, AssetStoreBuilder, StorageBackend},
    bufpool::{BytePool, PcmPool},
    play::ResourceConfig,
};
use libfuzzer_sys::fuzz_target;

static STORE: LazyLock<AssetStore> = LazyLock::new(|| {
    AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build()
});

#[derive(Arbitrary, Debug)]
struct Input {
    raw: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let mut raw = input.raw;
    raw.truncate(4 * 1024);

    let text = String::from_utf8_lossy(&raw);
    let _ = ResourceConfig::new(
        text.as_ref(),
        STORE.clone(),
        BytePool::default(),
        PcmPool::default(),
    );
});
