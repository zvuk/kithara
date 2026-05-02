//! Worst-case persistence tests: every test simulates a process
//! crash by either (a) skipping the explicit `checkpoint()` /
//! teardown or (b) externally mangling the on-disk artefacts a real
//! crash would leave behind. Each test then opens a fresh
//! `AssetStore` over the same cache dir and asserts the rebuilt state
//! is safe — never claims bytes that aren't durable, never panics, and
//! survives obviously bad inputs.
//!
//! The matrix:
//!   1. `pins.bin` truncated to zero / replaced with garbage
//!   2. `lru.bin` truncated to zero / replaced with garbage
//!   3. `availability.bin` truncated / garbage
//!   4. Segment file deleted out from under the store after checkpoint
//!   5. Segment partially written, no commit, no checkpoint
//!   6. Crash between `commit()` and `checkpoint()` — slow-path recovers
//!   7. Multi-store: per-store crash leaves siblings consistent
//!
//! These pin the contract: a crash anywhere in the persistence path
//! degrades gracefully to an empty index plus the on-disk slow-path
//! fallback; no hang, no panic, no decode-of-garbage.

#![cfg(not(target_arch = "wasm32"))]

use std::{fs, path::Path};

use kithara_assets::{AssetStoreBuilder, FlushHub, FlushPolicy, ResourceKey};
use kithara_platform::time::Duration;
use kithara_storage::ResourceExt;
use kithara_test_utils::kithara;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

struct Consts;

impl Consts {
    const ASSET_ROOT: &'static str = "crash-test";
    const KEY_NAME: &'static str = "segments/0001.bin";
}

fn pins_bin(root: &Path) -> std::path::PathBuf {
    root.join("_index/pins.bin")
}

fn lru_bin(root: &Path) -> std::path::PathBuf {
    root.join("_index/lru.bin")
}

fn availability_bin(root: &Path) -> std::path::PathBuf {
    root.join("_index/availability.bin")
}

fn segment_path(root: &Path) -> std::path::PathBuf {
    root.join(Consts::ASSET_ROOT).join(Consts::KEY_NAME)
}

/// Write a complete segment + flush every index file. Uses an
/// explicit `checkpoint()` so the on-disk state matches what a clean
/// shutdown would produce. The closure runs *after* checkpoint and
/// before drop — the place to inject a "crash" by mangling files.
fn seed_clean_state_then(dir: &Path, mangle: impl FnOnce(&Path)) {
    let store = AssetStoreBuilder::new()
        .root_dir(dir)
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();
    let key = ResourceKey::new(Consts::KEY_NAME);
    let res = store.acquire_resource(&key).unwrap();
    res.write_at(0, b"hello-world!").unwrap();
    res.commit(Some(12)).unwrap();
    drop(res);
    store.checkpoint().unwrap();
    mangle(dir);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn truncated_pins_bin_is_treated_as_empty() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        // Simulate a crash mid-write that left the file at zero bytes
        // (pre-S3 hazard, kept as a defence-in-depth assertion).
        fs::write(pins_bin(root), b"").unwrap();
    });

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();

    // Empty pin set; reopening must succeed and acquire-pin works.
    let key = ResourceKey::new(Consts::KEY_NAME);
    let _res = store
        .acquire_resource(&key)
        .expect("rebuild over zero-byte pins.bin must still acquire");
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn garbage_pins_bin_is_treated_as_empty() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        // Bytes that will fail rkyv validation. `read_pins` falls back
        // to `unwrap_or_default()`, never panics.
        fs::write(pins_bin(root), b"NOT-RKYV-PAYLOAD-AT-ALL").unwrap();
    });

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();
    let key = ResourceKey::new(Consts::KEY_NAME);
    let _res = store
        .acquire_resource(&key)
        .expect("garbage pins.bin must not block rebuild");
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn garbage_lru_bin_is_treated_as_empty() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::write(lru_bin(root), [0xff; 64]).unwrap();
    });

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();

    // Slow-path fallback still surfaces the committed segment — the
    // damaged LRU does not erase what's actually on disk.
    let key = ResourceKey::new(Consts::KEY_NAME);
    assert_eq!(store.final_len(&key), Some(12));
    assert!(store.contains_range(&key, 0..12));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn garbage_availability_bin_is_treated_as_empty() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::write(availability_bin(root), b"corrupted-bytes-here").unwrap();
    });

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();

    // The aggregate is empty (corrupt → default), but slow-path
    // `resource_state` still discovers the committed file on disk.
    let key = ResourceKey::new(Consts::KEY_NAME);
    assert_eq!(
        store.final_len(&key),
        Some(12),
        "slow-path must recover committed segments when availability.bin is unreadable"
    );
    assert!(store.contains_range(&key, 0..12));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn segment_deleted_externally_after_checkpoint_degrades_gracefully() {
    // Pin the **current** contract for an externally wiped segment
    // (antivirus, automated cleaner, etc.). The aggregate is
    // authoritative for `final_len`/`contains_range` after rebuild —
    // it does NOT re-verify each file against the filesystem on
    // hydration (that would scale poorly). So a stale claim survives
    // until the resource is actually opened.
    //
    // The acceptable failure mode is "a subsequent `acquire_resource`
    // surfaces the missing-bytes condition without panicking or
    // returning UB". This test pins both halves of that contract.
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::remove_file(segment_path(root)).unwrap();
    });

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();
    let key = ResourceKey::new(Consts::KEY_NAME);

    // Stale aggregate claim survives — documented degraded mode.
    assert_eq!(
        store.final_len(&key),
        Some(12),
        "aggregate is not re-verified against disk on hydration"
    );

    // The recovery path: when a reader actually tries to use the
    // resource the missing-file condition surfaces. `acquire_resource`
    // either re-creates an empty Active resource (recovery) or returns
    // an error — both are acceptable, the panic-free contract is what
    // matters here.
    match store.acquire_resource(&key) {
        Ok(res) => {
            // Re-acquired as a fresh resource. The previous claim is
            // implicitly invalidated by the new write path.
            let mut buf = Vec::new();
            let _ = res.read_into(&mut buf);
            // No panic — done.
        }
        Err(e) => {
            tracing::debug!(error = %e, "stale-claim resource correctly errored on acquire");
        }
    }
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn partial_segment_with_no_commit_and_no_checkpoint_is_invisible_after_crash() {
    let dir = tempdir().unwrap();
    let key = ResourceKey::new(Consts::KEY_NAME);

    // Phase 1: simulate a write-in-progress that gets killed before
    // commit OR checkpoint. The lease resource is dropped without
    // status=Committed, so disk_store::remove_resource cleans up. But
    // even if cleanup raced a kill -9 and left bytes on disk, the
    // index has not yet learned about them, so the rebuild path must
    // not surface partial bytes as a valid range.
    {
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some(Consts::ASSET_ROOT))
            .build();
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"partial-bytes").unwrap();
        // intentionally NO commit, NO checkpoint
        drop(res);
    }

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();

    // No claim must survive — the cache invariant is "the index never
    // points at bytes the filesystem doesn't have".
    assert!(store.available_ranges(&key).is_empty());
    assert_eq!(store.final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn commit_then_crash_before_checkpoint_recovers_via_slow_path() {
    let dir = tempdir().unwrap();
    let key = ResourceKey::new(Consts::KEY_NAME);

    // Commit succeeds → segment file is durable. Checkpoint NEVER
    // runs (process killed). availability.bin therefore reflects an
    // earlier (empty) snapshot. This is the most common real-world
    // crash window.
    {
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some(Consts::ASSET_ROOT))
            .build();
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"durable-data").unwrap();
        res.commit(Some(12)).unwrap();
        drop(res);
        // explicit: NO checkpoint
    }

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();

    // availability.bin doesn't exist (or is empty) yet — slow-path
    // must still find the committed file on disk and claim it.
    assert_eq!(store.final_len(&key), Some(12));
    assert!(store.contains_range(&key, 0..12));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn crash_between_per_store_flushes_keeps_each_store_independently_consistent() {
    // Two stores share one hub. Simulate crash mid-`flush_now`: store
    // A finishes its three indexes, then process dies before B's
    // indexes flush. On restart, A looks normal; B falls back to
    // slow-path. The sibling stores must NOT contaminate each other.
    let dir = tempdir().unwrap();
    let hub = FlushHub::new(CancellationToken::new(), FlushPolicy::default());

    let dir_a = dir.path().join("a");
    let dir_b = dir.path().join("b");

    let key = ResourceKey::new(Consts::KEY_NAME);

    {
        let store_a = AssetStoreBuilder::new()
            .root_dir(&dir_a)
            .asset_root(Some("track-a"))
            .flush_hub(hub.clone())
            .build();
        let store_b = AssetStoreBuilder::new()
            .root_dir(&dir_b)
            .asset_root(Some("track-b"))
            .flush_hub(hub.clone())
            .build();

        let res_a = store_a.acquire_resource(&key).unwrap();
        res_a.write_at(0, b"alpha-data!!").unwrap();
        res_a.commit(Some(12)).unwrap();
        drop(res_a);

        let res_b = store_b.acquire_resource(&key).unwrap();
        res_b.write_at(0, b"bravo-data!!").unwrap();
        res_b.commit(Some(12)).unwrap();
        drop(res_b);

        // Checkpoint store A only — simulate a crash that interrupted
        // before the second store's checkpoint ran.
        store_a.checkpoint().unwrap();
        // store_b dropped without checkpoint
    }

    let rebuilt_a = AssetStoreBuilder::new()
        .root_dir(&dir_a)
        .asset_root(Some("track-a"))
        .build();
    let rebuilt_b = AssetStoreBuilder::new()
        .root_dir(&dir_b)
        .asset_root(Some("track-b"))
        .build();

    assert_eq!(rebuilt_a.final_len(&key), Some(12));
    assert!(rebuilt_a.contains_range(&key, 0..12));
    assert_eq!(
        rebuilt_b.final_len(&key),
        Some(12),
        "sibling without checkpoint still recovers via slow-path"
    );
    assert!(rebuilt_b.contains_range(&key, 0..12));
}

// ----------------------------------------------------------------
// RED TESTS — demonstrate the contracts that S3 (atomic mmap commit
// for segments) is meant to enforce. These currently FAIL because
// segments are written directly to their canonical path: there is
// no temp-file-then-rename guarantee and no fsync before commit
// returns. After S3 they must turn green.
// ----------------------------------------------------------------

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn red_segment_file_must_not_be_visible_at_canonical_path_before_commit() {
    // Contract (post-S3): while a writer is filling a segment, its
    // canonical path on disk must not exist or must contain no
    // observable bytes. Readers using slow-path `fs::metadata` /
    // direct file open must see "no file" until `commit()` runs the
    // atomic rename.
    //
    // Current behaviour (RED): segment is mmap'd directly at the
    // canonical path. After even one `write_at`, the file is visible
    // with partial bytes — any external scanner / parallel reader
    // hitting it can deserialise garbage.
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();
    let key = ResourceKey::new(Consts::KEY_NAME);
    let res = store.acquire_resource(&key).unwrap();
    res.write_at(0, b"partial-bytes").unwrap();
    // explicitly NO commit yet — writer still in flight

    let canonical = segment_path(dir.path());
    let canonical_visible_with_bytes = canonical.metadata().map(|m| m.len() > 0).unwrap_or(false);
    assert!(
        !canonical_visible_with_bytes,
        "segment must not be observable at its canonical path before commit; \
         current state leaks partial bytes to any external reader"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn red_kill9_mid_write_must_not_leave_canonical_file_with_partial_bytes() {
    // Contract (post-S3): a writer killed (kill -9) mid-write must
    // not leave a file at the canonical path. The temp file may
    // exist for a startup cleanup pass to sweep, but the canonical
    // path must be empty so slow-path `resource_state` correctly
    // reports the resource as missing.
    //
    // Current behaviour (RED): writer's mmap is bound to the
    // canonical path. `mem::forget` simulates kill -9 by skipping
    // `LeaseResource::drop` cleanup. Result: canonical file with
    // partial bytes lingers and a fresh store sees it as a Committed
    // resource via slow-path.
    let dir = tempdir().unwrap();
    {
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some(Consts::ASSET_ROOT))
            .build();
        let key = ResourceKey::new(Consts::KEY_NAME);
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"partial-bytes-from-killed-writer")
            .unwrap();
        // Simulate kill -9: skip LeaseResource::drop cleanup.
        std::mem::forget(res);
        std::mem::forget(store);
    }

    let canonical = segment_path(dir.path());
    if canonical.exists() {
        let len = canonical.metadata().unwrap().len();
        assert_eq!(
            len, 0,
            "kill -9 mid-write must not leave a non-empty canonical segment file; \
             found {len} bytes at {canonical:?}"
        );
    }

    // Fresh store rebuild — slow-path must NOT classify the leaked
    // file as a Committed resource the reader will trust.
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();
    let key = ResourceKey::new(Consts::KEY_NAME);
    assert_eq!(
        store.final_len(&key),
        None,
        "after kill -9 mid-write, no resource state must claim the partial file"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn red_canonical_path_must_have_exact_bytes_after_commit_no_initial_mmap_padding() {
    // Contract (post-S3): after commit, the file at the canonical path
    // is exactly `final_len` bytes — neither padded with mmap initial
    // size nor truncated. This is the natural side-effect of
    // tempfile + rename: the temp file is grown via mmap, then on
    // commit the data is sync'd and the temp file (sized exactly to
    // `final_len` via ftruncate before rename) replaces the canonical.
    //
    // Current behaviour (RED): without atomic commit the mmap is
    // bound directly to the canonical path. The mmap is grown to a
    // power-of-two ≥ final_len; commit truncates back. But there is
    // no atomicity guarantee, and intermediate scanners that hit the
    // file during writes see partial data plus mmap-zero padding.
    let dir = tempdir().unwrap();
    let payload = b"exactly-12-b";
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();
    let key = ResourceKey::new(Consts::KEY_NAME);
    let res = store.acquire_resource(&key).unwrap();
    res.write_at(0, payload).unwrap();

    // Mid-write: the canonical path must not contain anything yet.
    let canonical = segment_path(dir.path());
    let mid_write_size = canonical.metadata().map(|m| m.len()).unwrap_or(0);
    assert_eq!(
        mid_write_size, 0,
        "mid-write the canonical path must contain zero observable bytes (got {mid_write_size})"
    );

    res.commit(Some(payload.len() as u64)).unwrap();
    drop(res);

    let on_disk = fs::read(&canonical).expect("canonical exists post-commit");
    assert_eq!(
        on_disk.len(),
        payload.len(),
        "post-commit canonical file must be exactly final_len bytes (no mmap padding)"
    );
    assert_eq!(on_disk.as_slice(), payload);
}

// ----------------------------------------------------------------
// END RED TESTS
// ----------------------------------------------------------------

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn doubly_corrupted_indexes_do_not_panic_and_slow_path_serves_data() {
    // Worst case: every index file is mangled (truncated, garbage,
    // mixed). The store must still come up and slow-path must still
    // find what is actually on disk.
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::write(pins_bin(root), b"").unwrap();
        fs::write(lru_bin(root), b"\xfe\xfe\xfe").unwrap();
        fs::write(availability_bin(root), b"PARTIAL!").unwrap();
    });

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some(Consts::ASSET_ROOT))
        .build();
    let key = ResourceKey::new(Consts::KEY_NAME);

    assert_eq!(
        store.final_len(&key),
        Some(12),
        "every index corrupt → slow-path still works"
    );
    assert!(store.contains_range(&key, 0..12));

    // And a fresh checkpoint must succeed on top of the cleaned state.
    store
        .checkpoint()
        .expect("checkpoint over cleaned state must succeed");
}
