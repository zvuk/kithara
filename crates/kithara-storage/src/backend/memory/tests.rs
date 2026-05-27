#![forbid(unsafe_code)]

mod kithara {
    pub(crate) use kithara_test_macros::test;
}

#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::thread;
use kithara_platform::time::Duration;
use tokio_util::sync::CancellationToken;

#[cfg(not(target_arch = "wasm32"))]
use crate::StorageError;
use crate::{
    backend::memory::driver::{MemOptions, MemResource},
    resource::{ResourceExt, ResourceStatus, WaitOutcome},
};

fn create_resource() -> MemResource {
    MemResource::new(CancellationToken::new())
}

fn with_bytes(data: &[u8], cancel: CancellationToken) -> MemResource {
    MemResource::open(
        cancel,
        MemOptions {
            initial_data: Some(data.to_vec()),
            ..MemOptions::default()
        },
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
fn test_write_and_read() {
    let res = create_resource();

    res.write_at(0, b"hello world").unwrap();
    res.commit(Some(11)).unwrap();

    let mut buf = [0u8; 11];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 11);
    assert_eq!(&buf, b"hello world");
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_write_all_read_into() {
    let res = create_resource();

    res.write_all(b"atomic data").unwrap();

    let mut buf = Vec::new();
    let n = res.read_into(&mut buf).unwrap();
    assert_eq!(n, 11);
    assert_eq!(&buf[..], b"atomic data");
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_from_bytes() {
    let res = with_bytes(b"preloaded", CancellationToken::new());

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
    let res2 = res.clone();

    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        res2.write_at(0, b"delayed data").unwrap();
    });

    let outcome = res.wait_range(0..12).unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);
    handle.join().unwrap();
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_wait_range_eof() {
    let res = create_resource();

    res.write_at(0, b"short").unwrap();
    res.commit(Some(5)).unwrap();

    let outcome = res.wait_range(5..10).unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);
}

#[kithara::test(native)]
fn test_fail_wakes_waiters() {
    let res = create_resource();
    let res2 = res.clone();

    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        res2.fail("test error".to_string());
    });

    let result = res.wait_range(0..100);
    assert!(result.is_err());
    handle.join().unwrap();
}

#[kithara::test(native)]
fn test_cancel_wakes_waiters() {
    let cancel = CancellationToken::new();
    let res = MemResource::new(cancel.clone());

    let handle = thread::spawn({
        let cancel = cancel.clone();
        move || {
            thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        }
    });

    let result = res.wait_range(0..100);
    assert!(matches!(result, Err(StorageError::Cancelled)));
    handle.join().unwrap();
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_status_transitions() {
    let res = create_resource();

    assert_eq!(res.status(), ResourceStatus::Active);

    res.write_at(0, b"data").unwrap();
    assert_eq!(res.status(), ResourceStatus::Active);

    res.commit(Some(4)).unwrap();
    assert_eq!(
        res.status(),
        ResourceStatus::Committed { final_len: Some(4) }
    );
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_status_failed() {
    let res = create_resource();

    res.fail("boom".to_string());
    assert_eq!(res.status(), ResourceStatus::Failed("boom".to_string()));
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_reactivate() {
    let res = create_resource();

    res.write_at(0, b"hello").unwrap();
    res.commit(Some(5)).unwrap();
    assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

    res.reactivate().unwrap();
    assert_eq!(res.status(), ResourceStatus::Active);
    assert_eq!(res.len(), None);

    let mut buf = [0u8; 5];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"hello");

    res.write_at(5, b" world").unwrap();
    res.commit(Some(11)).unwrap();

    let mut buf2 = vec![0u8; 11];
    let n = res.read_at(0, &mut buf2).unwrap();
    assert_eq!(n, 11);
    assert_eq!(&buf2[..], b"hello world");
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn test_write_rejected_after_commit() {
    let res = create_resource();
    res.write_at(0, b"data").unwrap();
    res.commit(Some(4)).unwrap();

    let result = res.write_at(0, b"nope");
    assert!(result.is_err());
}

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
    let res = MemResource::open(
        CancellationToken::new(),
        MemOptions {
            capacity: 64,
            ..Default::default()
        },
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
    let res = with_bytes(data, CancellationToken::new());

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
