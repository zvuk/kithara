//! Auto-checkpoint of `_index/availability.bin`.
//!
//! Invariant: in production callers (`Stream<Hls>`, file playback, etc.) nobody
//! explicitly invokes [`AssetStore::checkpoint`]. These tests pin the two
//! implicit trigger points:
//!
//! - **commit-count debounce**: after every `checkpoint_every` committed
//!   resources the store auto-persists the aggregate, so a crash-restart
//!   reloads byte-availability without scanning every segment file;
//! - **cancel-token shutdown**: on cooperative shutdown the store flushes the
//!   aggregate as its final act.
//!
//! Without these hooks `_index/availability.bin` is never written in the real
//! app lifecycle (see `pins.bin` / `lru.bin` which get eager writes on each
//! mutation and therefore DO exist on disk).

use std::num::NonZeroUsize;

use kithara_assets::{AssetStoreBuilder, ResourceKey};
use kithara_platform::{
    time::{Duration, Instant},
    tokio::time::sleep,
};
use kithara_storage::ResourceExt;
use kithara_test_utils::kithara;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

/// Poll for the availability index file to appear on disk (bounded wait).
async fn wait_for_availability_file(path: &std::path::Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while !path.exists() && Instant::now() < deadline {
        sleep(Duration::from_millis(20)).await;
    }
    path.exists()
}

/// After `checkpoint_every` committed resources the aggregate must land on
/// disk without any explicit `checkpoint()` call. This mirrors the real app
/// lifecycle: commits trickle in during playback and nobody drives an explicit
/// flush.
#[kithara::test(native, tokio, timeout(Duration::from_secs(5)))]
async fn disk_auto_checkpoint_persists_after_threshold() {
    let dir = tempdir().unwrap();
    let cancel = CancellationToken::new();
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("auto-persist"))
        .cancel(cancel.clone())
        .checkpoint_every(NonZeroUsize::new(1).unwrap())
        .build();

    let key = ResourceKey::new("segments/0001.bin");
    {
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"hello world").unwrap();
        res.commit(Some(11)).unwrap();
    }

    let availability_path = dir.path().join("_index").join("availability.bin");
    let appeared = wait_for_availability_file(&availability_path, Duration::from_secs(2)).await;
    assert!(
        appeared,
        "availability.bin must be auto-written after commit (checkpoint_every=1)",
    );

    // Rebuild over the same cache dir — aggregate must be seeded from the
    // auto-flushed snapshot, not from the slow `resource_state` fallback.
    let store2 = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("auto-persist"))
        .build();
    assert_eq!(store2.final_len(&key), Some(11));
    assert!(store2.contains_range(&key, 0..11));
}

/// Shutdown path: cooperative cancellation of the store's cancel token flushes
/// the aggregate even when the commit-count threshold has not been reached.
#[kithara::test(native, tokio, timeout(Duration::from_secs(5)))]
async fn disk_cancel_token_flushes_availability_on_shutdown() {
    let dir = tempdir().unwrap();
    let cancel = CancellationToken::new();
    // Threshold deliberately large — only cancel-driven flush should fire.
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("cancel-persist"))
        .cancel(cancel.clone())
        .checkpoint_every(NonZeroUsize::new(1000).unwrap())
        .build();

    let key = ResourceKey::new("segments/0001.bin");
    {
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"hi").unwrap();
        res.commit(Some(2)).unwrap();
    }

    let availability_path = dir.path().join("_index").join("availability.bin");

    // Threshold not reached — aggregate is still in memory only.
    assert!(
        !availability_path.exists(),
        "availability.bin should not exist yet (threshold=1000, commits=1)",
    );

    // Simulate cooperative shutdown.
    cancel.cancel();

    let appeared = wait_for_availability_file(&availability_path, Duration::from_secs(2)).await;
    assert!(
        appeared,
        "availability.bin must be flushed on cancel-token shutdown",
    );

    let store2 = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("cancel-persist"))
        .build();
    assert_eq!(store2.final_len(&key), Some(2));
}

/// `checkpoint_every = None` (the current default) must keep the historical
/// behaviour: no background flush, file only exists after explicit
/// `AssetStore::checkpoint()`.
#[kithara::test(native, tokio, timeout(Duration::from_secs(5)))]
async fn disk_auto_checkpoint_disabled_by_default() {
    let dir = tempdir().unwrap();
    let cancel = CancellationToken::new();
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("opt-in"))
        .cancel(cancel.clone())
        .build();

    let key = ResourceKey::new("segments/0001.bin");
    {
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"x").unwrap();
        res.commit(Some(1)).unwrap();
    }

    // Give any (accidental) background flusher a chance to run.
    sleep(Duration::from_millis(100)).await;

    let availability_path = dir.path().join("_index").join("availability.bin");
    assert!(
        !availability_path.exists(),
        "without checkpoint_every, auto-flush must stay disabled",
    );
}
