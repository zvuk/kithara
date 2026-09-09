#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use kithara::{
    assets::DiskAssetStore,
    platform::{CancelToken, time::Duration},
};
use kithara_integration_tests::{asset_fixture::PinsIndex, bufpool_ext::pools, temp_dir};

fn pins_path(root: &Path) -> PathBuf {
    root.join("_index").join("pins.bin")
}

#[kithara::fixture]
fn disk_asset_store(temp_dir: kithara_integration_tests::TestTempDir) -> DiskAssetStore {
    DiskAssetStore::new(temp_dir.path(), CancelToken::never())
}

#[derive(Clone, Copy)]
enum ReadBack {
    SameIndex,
    NewIndex,
    NewStore,
}

#[kithara::test(native, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
#[case::missing_file(None)]
#[case::corrupted_file(Some(&b"{ this is not valid json"[..]))]
fn pins_index_bad_state_returns_default(
    temp_dir: kithara_integration_tests::TestTempDir,
    disk_asset_store: DiskAssetStore,
    #[case] prewrite_contents: Option<&[u8]>,
) {
    let dir = temp_dir.path();
    let base = disk_asset_store;
    let path = pins_path(dir);

    match prewrite_contents {
        None => {
            assert!(!path.exists(), "pins.bin must not exist initially");
        }
        Some(bytes) => {
            fs::create_dir_all(dir.join("_index")).unwrap();
            fs::write(&path, bytes).unwrap();
            assert!(path.exists(), "pins.bin must exist for this test");
        }
    }

    let idx = PinsIndex::open(&base, &pools()).unwrap();
    let pins = idx.load().unwrap();

    assert!(
        pins.is_empty(),
        "bad pins index must be treated as empty (best-effort default)"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
#[case::new_index(vec!["asset-a", "asset-b"], ReadBack::NewIndex)]
#[case::single(vec!["asset-a"], ReadBack::SameIndex)]
#[case::three(vec!["asset-a", "asset-b", "asset-c"], ReadBack::SameIndex)]
#[case::five(
    vec!["asset-1", "asset-2", "asset-3", "asset-4", "asset-5"],
    ReadBack::SameIndex,
)]
#[case::empty(Vec::new(), ReadBack::SameIndex)]
#[case::new_store(
    vec!["persisted-asset", "another-asset"],
    ReadBack::NewStore,
)]
fn pins_index_roundtrip(
    #[case] asset_names: Vec<&str>,
    #[case] read_back: ReadBack,
    temp_dir: kithara_integration_tests::TestTempDir,
    disk_asset_store: DiskAssetStore,
) {
    let pins: HashSet<String> = asset_names.iter().map(ToString::to_string).collect();

    let loaded = match read_back {
        ReadBack::SameIndex => {
            let idx = PinsIndex::open(&disk_asset_store, &pools()).unwrap();
            idx.store(&pins).unwrap();
            idx.load().unwrap()
        }
        ReadBack::NewIndex => {
            let idx = PinsIndex::open(&disk_asset_store, &pools()).unwrap();
            idx.store(&pins).unwrap();
            PinsIndex::open(&disk_asset_store, &pools())
                .unwrap()
                .load()
                .unwrap()
        }
        ReadBack::NewStore => {
            let cancel = CancelToken::never();
            let base = DiskAssetStore::new(temp_dir.path(), cancel.clone());
            let idx = PinsIndex::open(&base, &pools()).unwrap();
            idx.store(&pins).unwrap();
            let reopened = DiskAssetStore::new(temp_dir.path(), cancel);
            PinsIndex::open(&reopened, &pools())
                .unwrap()
                .load()
                .unwrap()
        }
    };

    assert_eq!(loaded, pins, "pins index must roundtrip via store/load");
}

#[kithara::test(native, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
#[case(2)]
#[case(3)]
#[case(5)]
fn pins_index_concurrent_updates_handled_correctly(
    #[case] asset_count: usize,
    temp_dir: kithara_integration_tests::TestTempDir,
    disk_asset_store: DiskAssetStore,
) {
    let _dir = temp_dir.path();
    let base = disk_asset_store;

    let idx1 = PinsIndex::open(&base, &pools()).unwrap();
    let pins1: HashSet<String> = (0..asset_count)
        .map(|i| format!("asset-{}", i + 1))
        .collect();
    idx1.store(&pins1).unwrap();

    let idx2 = PinsIndex::open(&base, &pools()).unwrap();
    let loaded1 = idx2.load().unwrap();
    assert_eq!(loaded1, pins1);

    let pins2: HashSet<String> = (0..asset_count)
        .map(|i| format!("asset-updated-{}", i + 1))
        .collect();
    idx2.store(&pins2).unwrap();

    let idx3 = PinsIndex::open(&base, &pools()).unwrap();
    let loaded2 = idx3.load().unwrap();
    assert_eq!(loaded2, pins2);
}
