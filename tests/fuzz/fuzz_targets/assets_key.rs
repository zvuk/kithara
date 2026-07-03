#![no_main]

use arbitrary::Arbitrary;
use kithara_assets::{AssetStoreBuilder, ResourceInfo, asset_root_for_url};
use libfuzzer_sys::fuzz_target;
use url::Url;

#[derive(Arbitrary, Debug)]
struct Input {
    name: Option<Vec<u8>>,
    raw: Vec<u8>,
}

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

    let root = asset_root_for_url(&url, name.as_deref());
    assert_eq!(root.len(), 32);
    assert!(root.bytes().all(|b| b.is_ascii_hexdigit()));

    let store = AssetStoreBuilder::default().ephemeral(true).build();
    let scope = store.scope(root.clone());
    let key = scope.key_for(&ResourceInfo::Track {
        url: &url,
        name: name.as_deref(),
        ext_hint: None,
    });
    assert!(!key.is_absolute());

    let mirror = scope.key_for(&ResourceInfo::Manifest {
        url: &url,
        rendition: None,
    });
    let rel = mirror.rel_path().expect("manifest key is relative");
    assert!(rel.is_ascii());
    assert!(
        rel.split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
    );

    if url.host().is_some() {
        let mut without_query = url.clone();
        without_query.set_fragment(None);
        without_query.set_query(None);

        let root_without_query = asset_root_for_url(&without_query, name.as_deref());
        assert_eq!(root, root_without_query);
    }
});
