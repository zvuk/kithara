//! Hostile custom layout output must be rejected at the scope/key boundary.

use kithara_assets::{
    AssetLayout, AssetLayoutRegistry, AssetResource, AssetSource, AssetStore, AssetStoreBuilder,
    AssetsError, StorageBackend,
};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_test_utils::kithara;
use tempfile::tempdir;
use url::Url;

#[derive(Debug)]
struct HostileLayout {
    root: &'static str,
    path: &'static str,
}

impl AssetLayout for HostileLayout {
    fn root(&self, _source: &AssetSource) -> String {
        self.root.to_owned()
    }

    fn path(&self, _resource: &AssetResource) -> String {
        self.path.to_owned()
    }
}

struct HostileProtocol;

fn source() -> AssetSource {
    AssetSource::Remote {
        url: Url::parse("https://assets.test.invalid/track").expect("valid test source URL"),
        discriminator: Some("hostile-layout".to_owned()),
    }
}

fn store(layout: HostileLayout) -> (tempfile::TempDir, AssetStore) {
    let dir = tempdir().expect("test cache directory");
    let layouts = AssetLayoutRegistry::default().with::<HostileProtocol>(Arc::new(layout));
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .layouts(layouts)
        .build();
    (dir, store)
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
#[case("")]
#[case(".")]
#[case("..")]
#[case("../escape")]
#[case("a/b")]
#[case("/absolute")]
#[case("_index")]
#[case("_INDEX")]
#[case("con")]
fn hostile_layout_root_is_rejected(#[case] hostile: &'static str) {
    let (_dir, store) = store(HostileLayout {
        root: hostile,
        path: "resource/file.bin",
    });

    let err = store
        .scope::<HostileProtocol>(&source())
        .expect_err("hostile root must be rejected while creating the scope");
    assert!(matches!(err, AssetsError::InvalidKey));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
#[case("")]
#[case("../escape")]
#[case("/absolute/path")]
#[case("dir/../escape")]
#[case("dir//file")]
#[case("dir\\file")]
#[case("con/file")]
#[case("resource.tmp/file")]
#[case("resource.tmp")]
#[case("resource.TMP")]
fn hostile_layout_path_is_rejected(#[case] hostile: &'static str) {
    let (_dir, store) = store(HostileLayout {
        root: "valid-root",
        path: hostile,
    });
    let scope = store
        .scope::<HostileProtocol>(&source())
        .expect("valid layout root");
    let resource = AssetResource::Named {
        namespace: "resource".to_owned(),
        name: "file.bin".to_owned(),
    };

    let err = scope
        .key(&resource)
        .expect_err("hostile path must be rejected while creating the key");
    assert!(matches!(err, AssetsError::InvalidKey));
}
