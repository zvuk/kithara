#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

#[cfg(feature = "flash")]
use std::sync::{atomic::AtomicBool, mpsc};
use std::{
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

mod kithara {
    pub(crate) use kithara_test_macros::test;
}

use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
#[cfg(feature = "flash")]
use kithara_platform::{flash, tokio::task};
use tempfile::TempDir;

use super::core::{AtomicChunked, OpenIntent, make_tmp_path};
use crate::{
    AvailabilityObserver, MmapDriver, MmapOptions, OpenMode, Resource, ResourceRead,
    ResourceStatus, StorageError, StorageResult, consts,
};

fn open_chunked(dir: &TempDir, name: &str) -> (AtomicChunked<MmapDriver>, PathBuf, PathBuf) {
    let canonical = dir.path().join(name);
    let res = open_chunked_result(dir, name).unwrap();
    let tmp = make_tmp_path(&canonical).unwrap();
    (res, canonical, tmp)
}

/// Open the way the shipped path opens.
///
/// The claim protocol asserted here reads the same under either barrier, but
/// `open` puts a `sync_data` in front of every rename and this module is its
/// only caller in the workspace: `kithara-assets` opens the disk backend
/// deferred. So a test that commits twice buys two medium-speed durability
/// waits that nothing here asserts, and on a contended disk that is what
/// carries it past a budget its ten siblings share. The barrier keeps its
/// coverage where it is actually reachable: the concurrent-claim sibling
/// still opens through `open`, and never commits.
fn open_chunked_result(dir: &TempDir, name: &str) -> StorageResult<AtomicChunked<MmapDriver>> {
    let canonical = dir.path().join(name);
    let cancel = CancelToken::never();
    AtomicChunked::<MmapDriver>::open_deferred(canonical, move |target, intent| {
        let mode = match intent {
            OpenIntent::Fresh => OpenMode::ReadWrite,
            OpenIntent::Reopen => OpenMode::ReadOnly,
        };
        Resource::open(
            cancel.clone(),
            MmapOptions::for_path(target.to_path_buf())
                .mode(mode)
                .build(),
        )
    })
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn canonical_invisible_until_commit() {
    let dir = TempDir::new().unwrap();
    let (res, canonical, tmp) = open_chunked(&dir, "seg.bin");

    res.write_at(0, b"chunk-1-").unwrap();
    res.write_at(8, b"chunk-2!").unwrap();

    assert!(
        !canonical.exists(),
        "canonical must not exist before commit"
    );
    assert!(tmp.exists(), "tmp file must hold in-flight bytes");

    res.commit(Some(16)).unwrap();
    assert!(canonical.exists(), "canonical materialised on commit");
    assert!(!tmp.exists(), "tmp consumed by atomic rename");

    let bytes = fs::read(&canonical).unwrap();
    assert_eq!(&bytes, b"chunk-1-chunk-2!");
}

#[kithara::test(timeout(Duration::from_secs(2)))]
#[case::unknown(None, 64 * 1024)]
#[case::prefix(Some(7), 7)]
#[case::empty(Some(0), 0)]
fn released_backing_is_published_at_the_requested_extent(
    #[case] final_len: Option<u64>,
    #[case] expected_len: u64,
) {
    let dir = TempDir::new().unwrap();
    let (resource, canonical, tmp) = open_chunked(&dir, "final-extent.bin");
    resource.write_at(0, b"payload-tail").unwrap();

    resource.commit(final_len).unwrap();

    assert!(!tmp.exists());
    let bytes = fs::read(&canonical).unwrap();
    assert_eq!(bytes.len() as u64, expected_len);
    let prefix_len = bytes.len().min(12);
    assert_eq!(&bytes[..prefix_len], &b"payload-tail"[..prefix_len]);
    let mut observed = vec![0; prefix_len];
    assert_eq!(resource.read_at(0, &mut observed).unwrap(), prefix_len);
    assert_eq!(observed, bytes[..prefix_len]);
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn drop_without_commit_cleans_tmp() {
    let dir = TempDir::new().unwrap();
    let (res, canonical, tmp) = open_chunked(&dir, "abandoned.bin");

    res.write_at(0, b"will-not-commit").unwrap();
    assert!(tmp.exists());
    drop(res);

    assert!(!tmp.exists(), "Drop must remove the orphaned tmp");
    assert!(!canonical.exists(), "canonical must never appear");
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn fail_cleans_tmp() {
    let dir = TempDir::new().unwrap();
    let (res, canonical, tmp) = open_chunked(&dir, "failed.bin");

    res.write_at(0, b"oops").unwrap();
    res.fail("test".into());

    assert!(!tmp.exists(), "fail() must remove the tmp");
    assert!(!canonical.exists());
}

/// A tmp nobody holds is an orphan: its owner is gone, so nothing will ever
/// commit or clean it. Rejecting it would make the canonical path
/// permanently unwritable — the segment stays unfetchable for good.
#[kithara::test(timeout(Duration::from_secs(2)))]
fn open_reclaims_a_stale_tmp_no_live_writer_holds() {
    let dir = TempDir::new().unwrap();
    let stale_tmp = make_tmp_path(&dir.path().join("survivor.bin")).unwrap();
    fs::write(&stale_tmp, b"stale-from-previous-process").unwrap();

    let (res, canonical, tmp) = open_chunked(&dir, "survivor.bin");
    assert_eq!(tmp, stale_tmp, "the reclaimed tmp must be the planted one");

    res.write_at(0, b"written-by-successor").unwrap();
    res.commit(Some(20)).unwrap();
    assert_eq!(
        &fs::read(&canonical).unwrap(),
        b"written-by-successor",
        "the committed bytes must be the successor's, not the dead writer's"
    );
    assert!(!tmp.exists(), "tmp consumed by the atomic rename");
}

/// Reclaiming a stale tmp starts from an empty file. `create_new` used to
/// give that for free; the lock-based claim has to do it itself. The bytes are
/// read through a mapping because the claim's lock refuses plain reads through
/// any other handle on Windows.
#[kithara::test(timeout(Duration::from_secs(2)))]
fn a_reclaimed_tmp_carries_no_byte_of_its_dead_owner() {
    let dir = TempDir::new().unwrap();
    let stale_tmp = make_tmp_path(&dir.path().join("tail.bin")).unwrap();
    fs::write(&stale_tmp, consts::DEAD_OWNERS_BYTES).unwrap();

    let (_res, _canonical, tmp) = open_chunked(&dir, "tail.bin");

    let reader = Resource::<_, MmapDriver>::open(
        CancelToken::never(),
        MmapOptions::for_path(tmp).mode(OpenMode::ReadOnly).build(),
    )
    .unwrap();
    let mut bytes = vec![0xff; consts::DEAD_OWNERS_BYTES.len()];
    let read = reader.read_at(0, &mut bytes).unwrap();
    assert!(
        bytes[..read].iter().all(|byte| *byte == 0),
        "reclaimed tmp still carries the dead owner's bytes: {:?}",
        &bytes[..read]
    );
}

/// Emptiness is the invariant the driver reads the tmp against:
/// `MmapDriver::open` adopts a non-empty file whole — available `0..len`,
/// committed, `final_len` set — so a dead owner's bytes come back as readable
/// payload, not as residue nothing can reach.
#[kithara::test(timeout(Duration::from_secs(2)))]
fn a_reclaimed_tmp_offers_none_of_its_dead_owner_as_payload() {
    let dir = TempDir::new().unwrap();
    let stale_tmp = make_tmp_path(&dir.path().join("adopted.bin")).unwrap();
    fs::write(&stale_tmp, consts::DEAD_OWNERS_BYTES).unwrap();

    let (res, _canonical, _tmp) = open_chunked(&dir, "adopted.bin");

    assert!(
        !res.contains_range(0..consts::DEAD_OWNERS_BYTES.len() as u64),
        "the dead owner's bytes came back as available payload"
    );
}

/// Since the claim is a lock rather than a file, it has to be released when
/// the writer settles — otherwise a rewrite of the same canonical path would
/// see `TmpClaimed` from a holder that is done.
#[kithara::test(timeout(Duration::from_secs(2)))]
fn a_settled_claim_frees_the_tmp_for_a_successor() {
    let dir = TempDir::new().unwrap();
    let (holder, canonical, tmp) = open_chunked(&dir, "held.bin");
    holder.write_at(0, b"in-flight").unwrap();
    holder.commit(Some(9)).unwrap();
    drop(holder);
    assert!(!tmp.exists());
    assert!(canonical.is_file());

    let successor = open_chunked_result(&dir, "held.bin").expect("settled claim must be released");
    successor.write_at(0, b"rewritten").unwrap();
    successor.commit(Some(9)).unwrap();
    assert_eq!(&fs::read(&canonical).unwrap(), b"rewritten");
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn concurrent_open_atomic_claim_returns_tmp_claimed() {
    let dir = TempDir::new().unwrap();
    let canonical = dir.path().join("contested.bin");

    let cancel = CancelToken::never();
    let factory = {
        let cancel = cancel;
        move |target: &Path, intent: OpenIntent| {
            let mode = match intent {
                OpenIntent::Fresh => OpenMode::ReadWrite,
                OpenIntent::Reopen => OpenMode::ReadOnly,
            };
            Resource::open(
                cancel.clone(),
                MmapOptions::for_path(target.to_path_buf())
                    .mode(mode)
                    .build(),
            )
        }
    };

    let _holder = AtomicChunked::<MmapDriver>::open(canonical.clone(), factory.clone())
        .expect("first open claims tmp");

    let err = AtomicChunked::<MmapDriver>::open(canonical, factory)
        .expect_err("second concurrent open must be rejected");
    assert!(
        matches!(err, StorageError::TmpClaimed(_)),
        "expected TmpClaimed, got {err:?}"
    );
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn read_after_commit_returns_payload_via_decorator() {
    let dir = TempDir::new().unwrap();
    let (res, _, _) = open_chunked(&dir, "post-commit-read.bin");
    res.write_at(0, b"chunk-1-").unwrap();
    res.write_at(8, b"chunk-2!").unwrap();
    res.commit(Some(16)).unwrap();

    let mut buf = [0u8; 16];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 16, "post-commit read must return all bytes");
    assert_eq!(&buf, b"chunk-1-chunk-2!");

    let mut tail = [0u8; 1];
    let n = res.read_at(15, &mut tail).unwrap();
    assert_eq!(n, 1);
    assert_eq!(tail[0], b'!');
}

#[derive(Default)]
struct CommitCount(AtomicUsize);

impl AvailabilityObserver for CommitCount {
    fn on_commit(&self, _final_len: u64) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    fn on_write(&self, _range: Range<u64>) {}
}

#[derive(Default)]
struct CommitReadback {
    resource: Mutex<Weak<AtomicChunked<MmapDriver>>>,
    calls: AtomicUsize,
}

impl AvailabilityObserver for CommitReadback {
    fn on_commit(&self, _final_len: u64) {
        let resource = self.resource.lock().upgrade().unwrap();
        assert_eq!(resource.status(), ResourceStatus::Active);
        assert_eq!(resource.len(), None);
        let mut bytes = [0u8; 7];
        assert_eq!(resource.read_at(0, &mut bytes).unwrap(), bytes.len());
        assert_eq!(&bytes, b"payload");
        self.calls.fetch_add(1, Ordering::Relaxed);
    }

    fn on_write(&self, _range: Range<u64>) {}
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn a_commit_observer_reads_the_canonical_resource_before_readiness_is_published() {
    let dir = TempDir::new().unwrap();
    let observer = Arc::new(CommitReadback::default());
    let reopened = Arc::new(CommitCount::default());
    let fresh_observer = Arc::clone(&observer);
    let reopened_observer = Arc::clone(&reopened);
    let resource = Arc::new(
        AtomicChunked::<MmapDriver>::open_deferred(
            dir.path().join("reentrant.bin"),
            move |target, intent| {
                let (mode, observer): (OpenMode, Arc<dyn AvailabilityObserver>) = match intent {
                    OpenIntent::Fresh => (OpenMode::ReadWrite, Arc::clone(&fresh_observer) as _),
                    OpenIntent::Reopen => (OpenMode::ReadOnly, Arc::clone(&reopened_observer) as _),
                };
                Resource::open_with_observer(
                    CancelToken::never(),
                    MmapOptions::for_path(target.to_path_buf())
                        .mode(mode)
                        .build(),
                    Some(observer),
                )
            },
        )
        .unwrap(),
    );
    *observer.resource.lock() = Arc::downgrade(&resource);
    resource.write_at(0, b"payload").unwrap();

    resource.commit(Some(7)).unwrap();

    assert_eq!(observer.calls.load(Ordering::Relaxed), 1);
    assert_eq!(reopened.0.load(Ordering::Relaxed), 0);
    assert_eq!(
        resource.status(),
        ResourceStatus::Committed { final_len: Some(7) }
    );
}

#[kithara::test(timeout(Duration::from_secs(2)))]
#[case::rename(false)]
#[case::reopen(true)]
fn a_failed_atomic_publication_notifies_waiters_without_recording_a_commit(
    #[case] fail_reopen: bool,
) {
    let dir = TempDir::new().unwrap();
    let canonical = dir.path().join("failed-publication.bin");
    let commits = Arc::new(CommitCount::default());
    let observer = Arc::clone(&commits);
    let resource =
        AtomicChunked::<MmapDriver>::open_deferred(canonical.clone(), move |target, intent| {
            if fail_reopen && intent == OpenIntent::Reopen {
                return Err(StorageError::Failed("reopen refused".to_string()));
            }
            let mode = match intent {
                OpenIntent::Fresh => OpenMode::ReadWrite,
                OpenIntent::Reopen => OpenMode::ReadOnly,
            };
            Resource::open_with_observer(
                CancelToken::never(),
                MmapOptions::for_path(target.to_path_buf())
                    .mode(mode)
                    .build(),
                Some(Arc::clone(&observer) as Arc<dyn AvailabilityObserver>),
            )
        })
        .unwrap();
    resource.write_at(0, b"payload").unwrap();
    if !fail_reopen {
        fs::create_dir(&canonical).unwrap();
    }

    assert!(resource.commit(Some(7)).is_err());

    assert_eq!(commits.0.load(Ordering::Relaxed), 0);
    assert!(matches!(resource.status(), ResourceStatus::Failed(_)));
    assert!(matches!(
        resource.wait_range(7..8),
        Err(StorageError::Failed(_))
    ));
}

/// A Flash yield resolves only after spawned platform participants quiesce.
/// The captured fresh reader's only blocking operation is this range wait,
/// so an unfinished waiter after that yield is parked on the original core.
#[cfg(feature = "flash")]
#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
#[case::committed(false)]
#[case::failed(true)]
async fn atomic_publication_wakes_a_waiter_already_parked_on_the_original_core(
    #[case] fail_rename: bool,
) {
    let dir = TempDir::new().unwrap();
    let canonical = dir.path().join("parked-reader.bin");
    let (opened_tx, opened_rx) = mpsc::channel();
    let resource =
        AtomicChunked::<MmapDriver>::open_deferred(canonical.clone(), move |target, intent| {
            let mode = match intent {
                OpenIntent::Fresh => OpenMode::ReadWrite,
                OpenIntent::Reopen => OpenMode::ReadOnly,
            };
            let writer = Resource::open(
                CancelToken::never(),
                MmapOptions::for_path(target.to_path_buf())
                    .mode(mode)
                    .build(),
            )?;
            if intent == OpenIntent::Fresh {
                opened_tx.send(writer.reader()).unwrap();
            }
            Ok(writer)
        })
        .unwrap();
    let old_reader = opened_rx.try_recv().unwrap();
    assert_eq!(old_reader.status(), ResourceStatus::Active);
    resource.write_at(0, b"payload").unwrap();
    let returned = Arc::new(AtomicBool::new(false));
    let completed = Arc::clone(&returned);
    let waiter = task::spawn_blocking(move || {
        let result = old_reader.wait_range(7..8);
        completed.store(true, Ordering::Relaxed);
        result
    });

    flash::yield_now().await;
    assert!(!returned.load(Ordering::Relaxed));
    if fail_rename {
        fs::create_dir(&canonical).unwrap();
    }
    assert_eq!(resource.commit(Some(7)).is_err(), fail_rename);

    let outcome = waiter.await.unwrap();
    if fail_rename {
        assert!(matches!(outcome, Err(StorageError::Failed(_))));
    } else {
        assert_eq!(outcome.unwrap(), crate::WaitOutcome::Eof);
    }
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn read_during_writes_observes_inner_state() {
    let dir = TempDir::new().unwrap();
    let (res, _, _) = open_chunked(&dir, "live.bin");
    res.write_at(0, b"live-bytes").unwrap();
    let mut buf = [0u8; 10];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 10);
    assert_eq!(&buf, b"live-bytes");
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn passthrough_for_memory_inner_has_no_tmp() {
    let pools = crate::test_pools::pools();
    let mem = crate::MemResource::new(CancelToken::never(), crate::test_pools::byte_buffer(&pools));
    let res = AtomicChunked::passthrough(mem, PathBuf::from("virtual"));
    res.write_at(0, b"in-mem").unwrap();
    res.commit(Some(6)).unwrap();
    let mut buf = [0u8; 6];
    res.read_at(0, &mut buf).unwrap();
    assert_eq!(&buf, b"in-mem");
    assert_eq!(res.path(), Some(Path::new("virtual")));
}
