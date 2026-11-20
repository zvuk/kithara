#[cfg(not(target_arch = "wasm32"))]
use std::sync::Barrier;
use std::sync::atomic::{AtomicUsize, Ordering};

use kithara_platform::{CancelToken, sync::Arc, thread, time::Duration};
use kithara_storage::{
    MemOptions, MemResource, MmapOptions, MmapResource, Resource, ResourceStatus, StorageError,
    StorageResource, WaitOutcome,
};
use kithara_test_utils::kithara;
use tempfile::tempdir;

use super::{ChunkSink, ProcessCtx, ProcessedReader, ProcessedWriter, ResourceProcessor};
use crate::{
    AssetStore, StorageBackend,
    layout::ResourceKey,
    resource::{AcquisitionResult, BaseReader, BaseWriter, ReadSide, WriteSide},
};

fn mock_writer(content: &[u8]) -> (BaseWriter, tempfile::TempDir) {
    let dir = tempdir().expect("create processing test directory");
    let path = dir.path().join("test.bin");
    let cancel = CancelToken::never();

    let res: MmapResource = Resource::open(cancel, MmapOptions::for_path(path).build())
        .expect("open processing test resource");
    res.write_at(0, content)
        .expect("seed processing test resource");
    (BaseWriter::new(StorageResource::from(res)), dir)
}

fn mock_writer_mem(pools: &crate::test_pools::Pools, content: &[u8]) -> BaseWriter {
    let cancel = CancelToken::never();
    let res: MemResource = Resource::open(
        cancel,
        MemOptions::builder()
            .buffer(crate::test_pools::byte_buffer(pools))
            .build(),
    )
    .expect("open in-memory processing test resource");
    res.write_at(0, content)
        .expect("seed in-memory processing test resource");
    BaseWriter::new(StorageResource::from(res))
}

#[derive(Debug)]
struct XorProcessor {
    call_count: Arc<AtomicUsize>,
    identity: Box<[u8]>,
    xor_key: u8,
}

struct XorSink {
    call_count: Arc<AtomicUsize>,
    xor_key: u8,
}

impl ChunkSink for XorSink {
    fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        _is_last: bool,
    ) -> Result<usize, String> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        for (i, &b) in input.iter().enumerate() {
            output[i] = b ^ self.xor_key;
        }
        Ok(input.len())
    }
}

impl ResourceProcessor for XorProcessor {
    fn begin(&self) -> Box<dyn ChunkSink> {
        Box::new(XorSink {
            xor_key: self.xor_key,
            call_count: Arc::clone(&self.call_count),
        })
    }

    fn identity(&self) -> &[u8] {
        &self.identity
    }
}

fn xor_chunk_processor(xor_key: u8, call_count: Arc<AtomicUsize>) -> ProcessCtx {
    Arc::new(XorProcessor {
        xor_key,
        identity: Box::new([xor_key]),
        call_count,
    })
}

#[kithara::test]
fn writer_commit_returns_readable_reader() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
    let (writer, _dir) = mock_writer(b"test content");

    let writer = ProcessedWriter::builder()
        .inner(writer)
        .processor(process_fn)
        .pools(crate::test_pools::pools())
        .build();
    let reader = writer.commit(Some(b"test content".len() as u64)).unwrap();
    assert!(call_count.load(Ordering::SeqCst) > 0);

    let mut buf = vec![0u8; 12];
    let n = reader.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 12);
    let expected: Vec<u8> = b"test content".iter().map(|b| b ^ 0x42).collect();
    assert_eq!(buf, expected);
}

#[kithara::test]
fn read_at_after_processing_honours_offset() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0xff, Arc::clone(&call_count));
    let content: Vec<u8> = (0..100).collect();
    let (writer, _dir) = mock_writer(&content);

    let writer = ProcessedWriter::builder()
        .inner(writer)
        .processor(process_fn)
        .pools(crate::test_pools::pools())
        .build();
    let reader = writer.commit(Some(100)).unwrap();

    let mut buf = vec![0u8; 20];
    let n = reader.read_at(40, &mut buf).unwrap();
    assert_eq!(n, 20);
    let expected: Vec<u8> = (40..60).map(|b: u8| b ^ 0xff).collect();
    assert_eq!(buf, expected);
}

#[kithara::test]
fn ctx_none_writer_commits_straight_through_to_readable() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
    let (writer, _dir) = mock_writer(b"plain bytes!");

    let _ = process_fn;
    let writer: ProcessedWriter<BaseWriter, crate::test_pools::TestPools> =
        ProcessedWriter::builder()
            .inner(writer)
            .pools(crate::test_pools::pools())
            .build();
    let reader = writer.commit(Some(12)).unwrap();

    let mut buf = vec![0u8; 12];
    let n = reader.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 12);
    assert_eq!(&buf, b"plain bytes!", "ctx=None reads raw, unprocessed");
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "no processor runs for ctx=None"
    );
}

#[kithara::test]
fn encrypted_writer_reader_is_not_readable_before_commit() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
    let (writer, _dir) = mock_writer(b"test content");

    let writer = ProcessedWriter::builder()
        .inner(writer)
        .processor(process_fn)
        .pools(crate::test_pools::pools())
        .build();
    let reader = writer.reader();

    assert!(
        !reader.contains_range(0..12),
        "encrypted active resource must not advertise readable ranges before processing"
    );
    let mut buf = vec![0u8; 12];
    let err = reader
        .read_at(0, &mut buf)
        .expect_err("encrypted active resource must reject reads before commit");
    assert!(matches!(err, StorageError::NotReadable));
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn reader_view_blocks_until_writer_commits() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x55, Arc::clone(&call_count));
    let raw: Vec<u8> = (0..32u8).collect();
    let (writer, _dir) = mock_writer(&raw);
    let reopened = ProcessedReader::wrap_ready()
        .inner(writer.reader())
        .processor(Arc::clone(&process_fn))
        .pools(crate::test_pools::pools())
        .call();

    let writer = ProcessedWriter::builder()
        .inner(writer)
        .processor(process_fn)
        .pools(crate::test_pools::pools())
        .build();
    let raw_len = raw.len() as u64;

    let waiters = [writer.reader(), reopened].map(|reader| {
        thread::spawn(move || {
            let outcome = reader.wait_range(0..raw_len).unwrap();
            assert_eq!(outcome, WaitOutcome::Ready);
            let mut buf = vec![0u8; 32];
            reader.read_at(0, &mut buf).unwrap();
            buf
        })
    });

    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "process must not run before commit"
    );
    let _committed = writer.commit(Some(raw_len)).unwrap();

    let expected: Vec<u8> = (0..32u8).map(|b| b ^ 0x55).collect();
    for waiter in waiters {
        assert_eq!(
            waiter.join().unwrap(),
            expected,
            "every reader view must observe processed bytes"
        );
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn wait_range_aborts_on_cancellation() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));

    let dir = tempdir().unwrap();
    let path = dir.path().join("cancel.bin");
    let cancel = CancelToken::never();
    let resource: MmapResource =
        Resource::open(cancel.clone(), MmapOptions::for_path(path).build()).unwrap();
    resource.write_at(0, &[1u8; 16]).unwrap();

    let writer = ProcessedWriter::builder()
        .inner(BaseWriter::new(StorageResource::from(resource)))
        .processor(process_fn)
        .pools(crate::test_pools::pools())
        .build();
    let reader = writer.reader();

    let handle = thread::spawn(move || reader.wait_range(0..16));

    thread::sleep(Duration::from_millis(50));
    cancel.cancel();

    let outcome = handle
        .join()
        .expect("BUG: reader thread panicked")
        .expect("BUG: wait_range must not surface a hard error on cancel");
    assert_eq!(
        outcome,
        WaitOutcome::Interrupted,
        "cancellation must wake the gate as Interrupted, not block on the 100ms tick"
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "processor must not have run after cancellation"
    );
    // Keep the writer alive so cancellation, rather than drop, aborts the reader.
    drop(writer);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn external_cancel_interrupts_processing_wait_without_cancelling_resource() {
    let pools = crate::test_pools::pools();
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));
    let resource_cancel = CancelToken::never();
    let resource: MemResource = Resource::open(
        resource_cancel.clone(),
        MemOptions::builder()
            .buffer(crate::test_pools::byte_buffer(&pools))
            .build(),
    )
    .expect("open processing test resource");
    resource.write_at(0, &[1u8; 16]).unwrap();
    let writer = ProcessedWriter::builder()
        .inner(BaseWriter::new(StorageResource::from(resource)))
        .processor(process_fn)
        .pools(pools)
        .build();
    let reader = writer.reader();
    let wait_cancel = CancelToken::never();
    let entering_wait = Arc::new(Barrier::new(2));

    let handle = thread::spawn({
        let entering_wait = Arc::clone(&entering_wait);
        let wait_cancel = wait_cancel.clone();
        move || {
            entering_wait.wait();
            reader.wait_range_with_cancel(0..16, &wait_cancel)
        }
    });

    entering_wait.wait();
    wait_cancel.cancel();

    assert!(matches!(
        handle.join().expect("processing waiter must not panic"),
        Err(StorageError::Cancelled)
    ));
    assert!(!resource_cancel.is_cancelled());
    writer
        .commit(Some(16))
        .expect("resource remains committable");
    assert!(call_count.load(Ordering::SeqCst) > 0);
}

#[kithara::test]
fn reactivate_forks_fresh_gate_without_poisoning_reader() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
    let plaintext = b"hello drm world";
    let ciphertext: Vec<u8> = plaintext.iter().map(|b| b ^ 0x42).collect();
    let (writer, _dir) = mock_writer(&ciphertext);

    let writer = ProcessedWriter::builder()
        .inner(writer)
        .processor(process_fn)
        .pools(crate::test_pools::pools())
        .build();
    let reader_a = writer.commit(Some(ciphertext.len() as u64)).unwrap();

    let mut buf = vec![0u8; ciphertext.len()];
    reader_a.read_at(0, &mut buf).unwrap();
    assert_eq!(&buf[..], plaintext, "committed reader sees plaintext");

    let reader_b = reader_a.clone();
    let _writer_b = reader_a
        .reactivate()
        .expect("reactivate mints a fresh-gate writer");

    let mut buf2 = vec![0u8; ciphertext.len()];
    let n = reader_b
        .read_at(0, &mut buf2)
        .expect("reader clone must keep reading committed plaintext across a reactivate");
    assert_eq!(
        &buf2[..n],
        plaintext,
        "fresh-gate reactivate must not poison a prior-generation reader clone \
             (the structural root of live_ephemeral_small_cache_playback_drm)"
    );
}

#[kithara::test]
fn reactivate_then_commit_reruns_processor() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
    let first: Vec<u8> = b"first  payload".iter().map(|b| b ^ 0x42).collect();
    let second: Vec<u8> = b"second payload".iter().map(|b| b ^ 0x42).collect();
    assert_eq!(first.len(), second.len());
    let (writer, _dir) = mock_writer(&first);

    let writer = ProcessedWriter::builder()
        .inner(writer)
        .processor(process_fn)
        .pools(crate::test_pools::pools())
        .build();
    let len = first.len() as u64;
    let reader = writer.commit(Some(len)).expect("first commit");
    let first_count = call_count.load(Ordering::SeqCst);
    assert!(first_count > 0, "first commit runs the processor");

    let writer2 = reader.reactivate().expect("reactivate after commit");
    writer2.write_at(0, &second).expect("re-write ciphertext");
    let reader2 = writer2.commit(Some(len)).expect("second commit");
    assert!(
        call_count.load(Ordering::SeqCst) > first_count,
        "fresh gate -> second commit reruns the processor (decrypt not skipped)"
    );

    let mut out = vec![0u8; second.len()];
    reader2
        .read_at(0, &mut out)
        .expect("read after second commit");
    assert_eq!(&out[..], b"second payload");
}

#[kithara::test]
fn reactivate_then_commit_reruns_processor_mem() {
    let pools = crate::test_pools::pools();
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
    let first: Vec<u8> = b"first  payload".iter().map(|b| b ^ 0x42).collect();
    let second: Vec<u8> = b"second payload".iter().map(|b| b ^ 0x42).collect();
    assert_eq!(first.len(), second.len());
    let writer = mock_writer_mem(&pools, &first);

    let writer = ProcessedWriter::builder()
        .inner(writer)
        .processor(process_fn)
        .pools(pools)
        .build();
    let len = first.len() as u64;
    let reader = writer.commit(Some(len)).expect("first commit");
    let first_count = call_count.load(Ordering::SeqCst);
    assert!(first_count > 0, "first commit runs the processor");

    let writer2 = reader.reactivate().expect("reactivate after commit");
    writer2.write_at(0, &second).expect("re-write ciphertext");
    let reader2 = writer2.commit(Some(len)).expect("second commit");
    assert!(
        call_count.load(Ordering::SeqCst) > first_count,
        "fresh gate -> second commit reruns the processor (decrypt not skipped)"
    );

    let mut out = vec![0u8; second.len()];
    reader2
        .read_at(0, &mut out)
        .expect("read after second commit");
    assert_eq!(&out[..], b"second payload");
}

#[kithara::test(timeout(Duration::from_secs(5)))]
#[case::drop(false)]
#[case::abandon(true)]
fn ending_a_writer_without_commit_interrupts_its_readers(#[case] abandon: bool) {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));
    let (writer, _dir) = mock_writer(&[7u8; 16]);

    let writer = ProcessedWriter::builder()
        .inner(writer)
        .processor(process_fn)
        .pools(crate::test_pools::pools())
        .build();
    writer.write_at(0, &[7u8; 16]).unwrap();
    let reader = writer.reader();
    let reader_probe = reader.clone();

    let handle = thread::spawn(move || reader.wait_range(0..16));
    thread::sleep(Duration::from_millis(50));
    if abandon {
        writer.abandon();
    } else {
        drop(writer);
    }

    let outcome = handle
        .join()
        .expect("BUG: reader thread panicked")
        .expect("BUG: wait_range must not surface a hard error on gate fail");
    assert_eq!(
        outcome,
        WaitOutcome::Interrupted,
        "ending a writer without commit must wake a parked reader, not deadlock"
    );

    let mut buf = [0u8; 16];
    assert!(
        matches!(
            reader_probe.read_at(0, &mut buf),
            Err(StorageError::NotReadable)
        ),
        "a failed (never-committed) encrypted resource must reject read_at, \
             not return raw/partial bytes"
    );
}

#[kithara::test]
fn reopened_committed_processed_reader_is_readable_immediately() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
    let already_processed: Vec<u8> = b"test content".iter().map(|b| b ^ 0x42).collect();

    let dir = tempdir().unwrap();
    let path = dir.path().join("reopen.bin");
    let res: MmapResource =
        Resource::open(CancelToken::never(), MmapOptions::for_path(path).build()).unwrap();
    res.write_at(0, &already_processed).unwrap();
    let storage = StorageResource::from(res);
    storage
        .commit(Some(already_processed.len() as u64))
        .unwrap();

    let reader = ProcessedReader::wrap_ready()
        .inner(BaseReader::new(storage))
        .processor(process_fn)
        .pools(crate::test_pools::pools())
        .call();

    let mut buf = vec![0u8; already_processed.len()];
    let n = reader.read_at(0, &mut buf).unwrap();
    assert_eq!(n, already_processed.len());
    assert_eq!(buf, already_processed);
    assert_eq!(call_count.load(Ordering::SeqCst), 0);
}

#[kithara::test]
fn an_abandoned_generation_stays_unreadable_after_its_successor_commits() {
    let pools = crate::test_pools::pools();
    let processor = xor_chunk_processor(0x42, Arc::new(AtomicUsize::new(0)));
    let writer = ProcessedWriter::builder()
        .inner(mock_writer_mem(&pools, b"cipher"))
        .processor(processor)
        .pools(pools)
        .build();
    let reader = writer.reader();
    writer.abandon();
    assert!(
        matches!(reader.status(), ResourceStatus::Active),
        "abandon keeps the raw resource for a successor"
    );

    let committed = reader
        .clone()
        .reactivate()
        .expect("successor acquires a fresh processing gate")
        .commit(Some(6))
        .expect("successor commits processed bytes");
    let mut bytes = [0; 6];
    committed
        .read_at(0, &mut bytes)
        .expect("successor reads processed bytes");
    assert_eq!(bytes, (*b"cipher").map(|byte| byte ^ 0x42));
    assert!(
        matches!(
            reader.read_at(0, &mut bytes),
            Err(StorageError::NotReadable)
        ),
        "a successor commit cannot reopen the abandoned generation"
    );
}

#[kithara::test]
fn open_resource_none_ctx_does_not_leak_precommit_guard() {
    let store = AssetStore::builder(crate::test_pools::pools())
        .backend(StorageBackend::Memory)
        .build();
    let key = ResourceKey::relative("drm-fallthrough", "segment.m4s");
    let proc: ProcessCtx = xor_chunk_processor(0x42, Arc::new(AtomicUsize::new(0)));

    let AcquisitionResult::Pending(writer) = store
        .acquire_resource_with_ctx(&key, None, Some(Arc::clone(&proc)))
        .expect("BUG: acquire with ctx must succeed")
    else {
        panic!("fresh acquire must be Pending");
    };
    let payload = b"uncommitted encrypted bytes";
    writer
        .write_at(0, payload)
        .expect("BUG: writer must be able to stream bytes before commit");

    if let Ok(reader) = store.open_resource(&key, None) {
        let mut buf = vec![0u8; payload.len()];
        if let Err(err) = reader.read_at(0, &mut buf) {
            assert!(
                !matches!(err, StorageError::NotReadable),
                "read_at(ctx=None) leaked the pre-commit guard from a \
                     concurrent ctx=Some writer"
            );
        }
    }
}

/// The pass size is the caller's, not a crate constant: a commit over a
/// 32-byte resource runs one `ChunkSink::process` per configured chunk.
#[kithara::test]
fn a_small_chunk_size_splits_the_commit_into_passes() {
    const CONTENT_LEN: usize = 32;
    let pools = crate::test_pools::pools();
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
    let writer = ProcessedWriter::builder()
        .inner(mock_writer_mem(&pools, &[7u8; CONTENT_LEN]))
        .processor(process_fn)
        .pools(pools)
        .chunk_size(CONTENT_LEN / 4)
        .build();

    drop(writer.commit(Some(CONTENT_LEN as u64)).unwrap());

    assert_eq!(call_count.load(Ordering::SeqCst), 4);
}

#[kithara::test]
fn a_chunk_size_that_spans_the_resource_commits_in_one_pass() {
    const CONTENT_LEN: usize = 32;
    let pools = crate::test_pools::pools();
    let call_count = Arc::new(AtomicUsize::new(0));
    let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
    let writer = ProcessedWriter::builder()
        .inner(mock_writer_mem(&pools, &[7u8; CONTENT_LEN]))
        .processor(process_fn)
        .pools(pools)
        .chunk_size(CONTENT_LEN)
        .build();

    drop(writer.commit(Some(CONTENT_LEN as u64)).unwrap());

    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}
