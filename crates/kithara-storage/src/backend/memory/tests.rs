#![forbid(unsafe_code)]

mod kithara {
    pub(crate) use kithara_test_macros::test;
}

#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Barrier};

#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::thread;
use kithara_platform::{CancelToken, time::Duration};

#[cfg(not(target_arch = "wasm32"))]
use crate::StorageError;
use crate::{
    Driver, DriverIo, ResourceRead,
    backend::memory::driver::{MemDriver, MemOptions, MemResource},
    resource::{ResourceStatus, WaitOutcome},
    test_pools::{byte_buffer, pools, pools_with_budget},
};

fn create_resource() -> MemResource {
    let pools = pools();
    MemResource::new(CancelToken::never(), byte_buffer(&pools))
}

fn with_bytes(data: &[u8], cancel: CancelToken) -> MemResource {
    let pools = pools();
    MemResource::open(
        cancel,
        MemOptions::builder()
            .buffer(byte_buffer(&pools))
            .initial_data(data.to_vec())
            .build(),
    )
    .expect("BUG: MemDriver::open with initial_data is infallible")
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_create_new_resource() {
    let res = create_resource();
    assert_eq!(res.len(), None);
    assert_eq!(res.status(), ResourceStatus::Active);
    assert_eq!(res.path(), None);
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn post_commit_replacement_uses_injected_pool() {
    let pools = pools_with_budget(1024);
    let (driver, _) = MemDriver::open(
        MemOptions::builder()
            .buffer(pools.get::<u8>())
            .capacity(32)
            .build(),
    )
    .unwrap();
    DriverIo::commit(&driver, Some(0)).unwrap();
    let allocated_before = pools.stats().allocated_bytes;

    DriverIo::write_at(&driver, 0, &[1; 64], false).unwrap();

    assert!(pools.stats().allocated_bytes > allocated_before);
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn committed_resource_releases_working_capacity_to_the_shared_region() {
    const IDLE_BYTES: usize = 32;
    const WORKING_BYTES: usize = 64;
    const REGION_BYTES: usize = IDLE_BYTES + WORKING_BYTES;

    let pools = pools_with_budget(REGION_BYTES);
    let writer = MemResource::new(CancelToken::never(), pools.get::<u8>());
    writer.write_at(0, &[1; WORKING_BYTES]).unwrap();
    let idle = pools.get_with_len::<u8>(IDLE_BYTES).unwrap();
    drop(idle);
    let _reader = writer.commit(Some(WORKING_BYTES as u64)).unwrap();

    pools
        .get_with_len::<f32>(REGION_BYTES / size_of::<f32>())
        .expect("committed and idle working bytes must be reclaimable by another typed slot");
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_write_and_read() {
    let res = create_resource();

    res.write_at(0, b"hello world").unwrap();
    let res = res.commit(Some(11)).unwrap();

    let mut buf = [0u8; 11];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 11);
    assert_eq!(&buf, b"hello world");
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_write_all_read_into() {
    let res = create_resource();

    let res = res.write_all(b"atomic data").unwrap();

    let mut buf = Vec::new();
    let n = res.read_into(&mut buf).unwrap();
    assert_eq!(n, 11);
    assert_eq!(&buf[..], b"atomic data");
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_from_bytes() {
    let res = with_bytes(b"preloaded", CancelToken::never());

    assert_eq!(
        res.status(),
        ResourceStatus::Committed { final_len: Some(9) }
    );
    assert_eq!(res.len(), Some(9));

    let mut buf = Vec::new();
    let n = res.read_into(&mut buf).unwrap();
    assert_eq!(n, 9);
    assert_eq!(&buf[..], b"preloaded");
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_wait_range_ready() {
    let res = create_resource();

    res.write_at(0, b"data").unwrap();

    let outcome = res.wait_range(0..4).unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);
}

#[kithara::test(native)]
fn test_wait_range_blocks_then_ready() {
    let res = create_resource();
    let reader = res.reader();

    // Return the writer from the thread so it outlives the read: dropping an
    // uncommitted writer now marks the resource failed (anti-hang), which would
    // otherwise race the availability notify.
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        res.write_at(0, b"delayed data").unwrap();
        res
    });

    let outcome = reader.wait_range(0..12).unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);
    let _writer = handle.join().unwrap();
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_wait_range_eof() {
    let res = create_resource();

    res.write_at(0, b"short").unwrap();
    let res = res.commit(Some(5)).unwrap();

    let outcome = res.wait_range(5..10).unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);
}

#[kithara::test(native)]
fn test_fail_wakes_waiters() {
    let res = create_resource();
    let reader = res.reader();

    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        res.fail("test error".to_string());
    });

    let result = reader.wait_range(0..100);
    assert!(result.is_err());
    handle.join().unwrap();
}

#[kithara::test(native)]
fn test_cancel_wakes_waiters() {
    let pools = pools();
    let cancel = CancelToken::never();
    let res = MemResource::new(cancel.clone(), byte_buffer(&pools));

    let handle = thread::spawn({
        let cancel = cancel;
        move || {
            thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        }
    });

    let result = res.wait_range(0..100);
    assert!(matches!(result, Err(StorageError::Cancelled)));
    handle.join().unwrap();
}

#[kithara::test(native)]
fn external_cancel_wakes_waiter_without_cancelling_resource() {
    let pools = pools();
    let resource_cancel = CancelToken::never();
    let writer = MemResource::new(resource_cancel.clone(), byte_buffer(&pools));
    let reader = writer.reader();
    let wait_cancel = CancelToken::never();
    let entering_wait = Arc::new(Barrier::new(2));

    let handle = thread::spawn({
        let entering_wait = Arc::clone(&entering_wait);
        let wait_cancel = wait_cancel.clone();
        move || {
            entering_wait.wait();
            reader.wait_range_with_cancel(0..4, &wait_cancel)
        }
    });

    entering_wait.wait();
    wait_cancel.cancel();

    assert!(matches!(
        handle.join().expect("waiter thread must not panic"),
        Err(StorageError::Cancelled)
    ));
    assert!(!resource_cancel.is_cancelled());
    assert_eq!(writer.status(), ResourceStatus::Active);

    writer.write_at(0, b"done").unwrap();
    let committed = writer.commit(Some(4)).unwrap();
    let mut bytes = [0; 4];
    assert_eq!(committed.read_at(0, &mut bytes).unwrap(), 4);
    assert_eq!(&bytes, b"done");
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_status_transitions() {
    let res = create_resource();

    assert_eq!(res.status(), ResourceStatus::Active);

    res.write_at(0, b"data").unwrap();
    assert_eq!(res.status(), ResourceStatus::Active);

    let res = res.commit(Some(4)).unwrap();
    assert_eq!(
        res.status(),
        ResourceStatus::Committed { final_len: Some(4) }
    );
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_status_failed() {
    let res = create_resource();
    let reader = res.reader();

    res.fail("boom".to_string());
    assert_eq!(reader.status(), ResourceStatus::Failed("boom".to_string()));
}

/// The anti-hang stamp is what makes an abandoned resource poisonous to its
/// successor, so `abandon` must leave none: the caller releasing this way is
/// the one re-dispatching the write.
#[kithara::test(timeout(Duration::from_secs(1)))]
fn abandoned_writer_leaves_no_failure_stamp() {
    let res = create_resource();
    let reader = res.reader();

    res.abandon();
    drop(res);

    assert_eq!(reader.status(), ResourceStatus::Active);
}

/// `abandon` is an opt-in for a caller that owns the re-dispatch. Every other
/// release keeps the stamp, or a reader waits on bytes nobody will write.
#[kithara::test(timeout(Duration::from_secs(1)))]
fn dropped_writer_still_stamps_the_failure() {
    let res = create_resource();
    let reader = res.reader();

    drop(res);

    assert!(
        matches!(reader.status(), ResourceStatus::Failed(_)),
        "{:?}",
        reader.status()
    );
}

/// `abandon` waives the stamp for the writer that owns the refill, not for the
/// resource forever: the next write generation starts armed again.
#[kithara::test(timeout(Duration::from_secs(1)))]
fn reactivated_writer_is_armed_again_after_an_abandon() {
    let res = create_resource();
    res.abandon();
    let res = res.commit(Some(0)).unwrap();
    let res = res.reactivate().unwrap();
    let reader = res.reader();

    drop(res);

    assert!(
        matches!(reader.status(), ResourceStatus::Failed(_)),
        "{:?}",
        reader.status()
    );
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_reactivate() {
    let res = create_resource();

    res.write_at(0, b"hello").unwrap();
    let res = res.commit(Some(5)).unwrap();
    assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

    let res = res.reactivate().unwrap();
    assert_eq!(res.status(), ResourceStatus::Active);
    assert_eq!(res.len(), None);

    let mut buf = [0u8; 5];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"hello");

    res.write_at(5, b" world").unwrap();
    let res = res.commit(Some(11)).unwrap();

    let mut buf2 = vec![0u8; 11];
    let n = res.read_at(0, &mut buf2).unwrap();
    assert_eq!(n, 11);
    assert_eq!(&buf2[..], b"hello world");
}

// `test_write_rejected_after_commit` removed: writing after commit is now a
// compile error (`Resource<Committed, D>` has no `write_at`), not a runtime

#[kithara::test(timeout(Duration::from_secs(1)))]
#[case::sparse(100, b"sparse")]
#[case::growable_sparse(1000, b"far away")]
fn test_sparse_write(#[case] offset: u64, #[case] payload: &[u8]) {
    let res = create_resource();

    res.write_at(offset, payload).unwrap();

    let mut buf = vec![0u8; payload.len()];
    let n = res.read_at(offset, &mut buf).unwrap();
    assert_eq!(n, payload.len());
    assert_eq!(&buf[..], payload);

    let mut zero_buf = [0xFFu8; 4];
    let n = res.read_at(0, &mut zero_buf).unwrap();
    assert_eq!(n, 4);
    assert_eq!(&zero_buf, &[0, 0, 0, 0]);
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_growable_write_beyond_initial_capacity() {
    let pools = pools();
    let res = MemResource::open(
        CancelToken::never(),
        MemOptions::builder()
            .buffer(byte_buffer(&pools))
            .capacity(64)
            .build(),
    )
    .unwrap();

    let data = vec![0xAB; 128];
    res.write_at(0, &data).unwrap();

    let mut buf = vec![0u8; 128];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 128);
    assert!(buf.iter().all(|b| *b == 0xAB));
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_growable_multiple_writes_extend() {
    let res = create_resource();

    res.write_at(0, b"aaa").unwrap();
    res.write_at(3, b"bbb").unwrap();
    res.write_at(6, b"ccc").unwrap();

    let mut buf = [0u8; 9];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 9);
    assert_eq!(&buf, b"aaabbbccc");
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_from_bytes_readable() {
    let data = b"hello growable buffer world";
    let res = with_bytes(data, CancelToken::never());

    let mut buf = vec![0u8; data.len()];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, data.len());
    assert_eq!(&buf, data);
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_backward_write_does_not_lose_data() {
    let res = create_resource();

    res.write_at(0, &[0xAA; 100]).unwrap();
    res.write_at(200, &[0xBB; 100]).unwrap();
    res.write_at(50, &[0xCC; 50]).unwrap();

    let mut buf = [0u8; 10];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 10);
    assert_eq!(&buf, &[0xAA; 10]);

    let n = res.read_at(50, &mut buf).unwrap();
    assert_eq!(n, 10);
    assert_eq!(&buf, &[0xCC; 10]);

    let n = res.read_at(200, &mut buf).unwrap();
    assert_eq!(n, 10);
    assert_eq!(&buf, &[0xBB; 10]);
}
