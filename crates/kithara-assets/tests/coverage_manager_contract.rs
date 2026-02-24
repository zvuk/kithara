#![forbid(unsafe_code)]

use std::{ops::Range, time::Duration};

use kithara_assets::{AssetStore, AssetStoreBuilder};
use kithara_coverage::Coverage;
use kithara_test_utils::kithara;

fn mark_coverage(backend: &AssetStore, key: &str, total_size: u64, covered: Range<u64>) {
    let manager = backend
        .open_coverage_manager()
        .expect("coverage manager should open");
    let mut state = manager.open_state(key);
    state.set_total_size(total_size);
    state.mark(covered);
}

fn read_coverage(backend: &AssetStore, key: &str) -> (Option<u64>, Vec<Range<u64>>, bool) {
    let manager = backend
        .open_coverage_manager()
        .expect("coverage manager should open");
    let state = manager.open_state(key);
    (state.total_size(), state.gaps(), state.is_complete())
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn mem_backend_coverage_persists_between_manager_reopens() {
    let backend = AssetStoreBuilder::new()
        .ephemeral(true)
        .asset_root(Some("mem-coverage-contract"))
        .build();
    let key = "https://example.com/seg/v0_0.bin";

    mark_coverage(&backend, key, 128, 0..64);
    let (total, gaps, complete) = read_coverage(&backend, key);
    assert_eq!(total, Some(128));
    assert_eq!(gaps, vec![64..128]);
    assert!(!complete);

    mark_coverage(&backend, key, 128, 64..128);
    let (total, gaps, complete) = read_coverage(&backend, key);
    assert_eq!(total, Some(128));
    assert!(gaps.is_empty());
    assert!(complete);
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(5)))]
fn disk_backend_coverage_remove_invalidates_entry() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let backend = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root(Some("disk-coverage-contract"))
        .build();
    let key = "https://example.com/seg/v0_1.bin";

    mark_coverage(&backend, key, 256, 0..256);
    let (_, _, complete) = read_coverage(&backend, key);
    assert!(complete);

    let manager = backend
        .open_coverage_manager()
        .expect("coverage manager should open");
    manager.open_state(key).remove();

    let (total, gaps, complete) = read_coverage(&backend, key);
    assert_eq!(total, None);
    assert!(gaps.is_empty());
    assert!(!complete);
}
