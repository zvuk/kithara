#![no_main]

use arbitrary::Arbitrary;
use kithara::assets::{
    AssetLayout, AssetResource, AssetSource, AssetStoreBuilder, DefaultLayout, StorageBackend,
};
use libfuzzer_sys::fuzz_target;
use url::Url;

#[derive(Arbitrary, Debug)]
struct Input {
    name: Option<Vec<u8>>,
    raw: Vec<u8>,
}

struct FuzzProtocol;

fuzz_target!(|input: Input| {
    let mut raw = input.raw;
    raw.truncate(4 * 1024);

    let text = String::from_utf8_lossy(&raw);
    let Ok(url) = Url::parse(text.as_ref()) else {
        return;
    };

    let name = input
        .name
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned());

    if url.host().is_none() || url.path_segments().is_none() {
        return;
    }

    let source = AssetSource::Remote {
        url: url.clone(),
        discriminator: name.clone(),
    };
    let layout = DefaultLayout;
    let root = layout.root(&source);
    assert_eq!(root.len(), 32);
    assert!(root.bytes().all(|b| b.is_ascii_hexdigit()));

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build();
    let scope = store
        .scope::<FuzzProtocol>(&source)
        .expect("valid remote source");
    let key = scope
        .key(&AssetResource::Url(url.clone()))
        .expect("valid remote resource");
    assert!(!key.is_absolute());

    let rel = key.rel_path().expect("url key is relative");
    assert!(rel.is_ascii());
    assert!(
        rel.split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
    );

    let mut without_query = url;
    without_query.set_fragment(None);
    without_query.set_query(None);
    let source_without_query = AssetSource::Remote {
        url: without_query,
        discriminator: name,
    };
    assert_eq!(root, layout.root(&source_without_query));
});
