//! Tests for `Writer::with_offset()` — range-request resume path and `OffsetOverflow` error.

use bytes::Bytes;
use futures::StreamExt;
use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource, ResourceExt};
use kithara_stream::{Writer, WriterError, WriterItem};
use tokio_util::sync::CancellationToken;

/// Run a writer to completion, returning total bytes written.
async fn run_writer(
    mut writer: Writer<std::io::Error>,
) -> Result<u64, WriterError<std::io::Error>> {
    while let Some(result) = writer.next().await {
        match result {
            Ok(WriterItem::ChunkWritten { .. }) => {}
            Ok(WriterItem::StreamEnded { total_bytes }) => return Ok(total_bytes),
            Err(e) => return Err(e),
        }
    }
    Ok(0)
}

/// Helper: open a fresh `MmapResource` backed by a temp file.
#[expect(clippy::unwrap_used)]
fn open_resource(dir: &tempfile::TempDir, name: &str, len: u64) -> MmapResource {
    let path = dir.path().join(name);
    Resource::open(
        CancellationToken::new(),
        MmapOptions {
            path,
            initial_len: Some(len),
            mode: OpenMode::ReadWrite,
        },
    )
    .unwrap()
}

// 1. with_offset writes data at the correct position

#[tokio::test]
async fn writer_with_offset_writes_at_correct_position() {
    let dir = tempfile::tempdir().unwrap();
    let res = open_resource(&dir, "offset_write.bin", 1024);

    let start_offset: u64 = 100;
    let payload = vec![0xABu8; 512];
    let source = futures::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(
        payload.clone(),
    ))]);

    let cancel = CancellationToken::new();
    let mut writer: Writer<std::io::Error> =
        Writer::with_offset(source, res.clone(), cancel, start_offset);

    let mut saw_stream_ended = false;
    while let Some(result) = writer.next().await {
        match result.unwrap() {
            WriterItem::ChunkWritten { offset, len } => {
                assert_eq!(offset, 100);
                assert_eq!(len, 512);
            }
            WriterItem::StreamEnded { total_bytes } => {
                // total_bytes is the final offset (start_offset + bytes written)
                assert_eq!(total_bytes, 100 + 512);
                saw_stream_ended = true;
            }
        }
    }
    assert!(saw_stream_ended, "must receive StreamEnded");

    // Verify data actually landed at offset 100 in the resource.
    let mut buf = vec![0u8; 512];
    let n = res.read_at(100, &mut buf).unwrap();
    assert_eq!(n, 512);
    assert_eq!(buf, payload);
}

// 2. Multiple chunks written at correct cumulative offsets

#[tokio::test]
async fn writer_with_offset_multiple_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let res = open_resource(&dir, "multi_chunk.bin", 1024);

    let start_offset: u64 = 200;
    let chunk_a = vec![0xAAu8; 100];
    let chunk_b = vec![0xBBu8; 100];
    let chunk_c = vec![0xCCu8; 100];

    let source = futures::stream::iter(vec![
        Ok::<Bytes, std::io::Error>(Bytes::from(chunk_a.clone())),
        Ok::<Bytes, std::io::Error>(Bytes::from(chunk_b.clone())),
        Ok::<Bytes, std::io::Error>(Bytes::from(chunk_c.clone())),
    ]);

    let cancel = CancellationToken::new();
    let mut writer: Writer<std::io::Error> =
        Writer::with_offset(source, res.clone(), cancel, start_offset);

    let mut offsets = Vec::new();
    while let Some(result) = writer.next().await {
        match result.unwrap() {
            WriterItem::ChunkWritten { offset, len } => {
                offsets.push((offset, len));
            }
            WriterItem::StreamEnded { total_bytes } => {
                assert_eq!(total_bytes, 200 + 300);
            }
        }
    }

    assert_eq!(
        offsets,
        vec![(200, 100), (300, 100), (400, 100)],
        "each chunk must be written at start_offset + cumulative bytes"
    );

    // Read back and verify each chunk landed at the right place.
    let mut buf = vec![0u8; 100];

    res.read_at(200, &mut buf).unwrap();
    assert_eq!(buf, chunk_a);

    res.read_at(300, &mut buf).unwrap();
    assert_eq!(buf, chunk_b);

    res.read_at(400, &mut buf).unwrap();
    assert_eq!(buf, chunk_c);
}

// 3. with_offset(0) behaves identically to new()

#[tokio::test]
async fn writer_with_offset_zero_is_same_as_new() {
    let dir = tempfile::tempdir().unwrap();

    let payload = vec![0xFFu8; 256];

    // Writer::new
    let res_new = open_resource(&dir, "via_new.bin", 512);
    let source_new = futures::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(
        payload.clone(),
    ))]);
    let cancel_new = CancellationToken::new();
    let writer_new: Writer<std::io::Error> = Writer::new(source_new, res_new.clone(), cancel_new);
    let total_new = run_writer(writer_new).await.unwrap();

    // Writer::with_offset(0)
    let res_offset = open_resource(&dir, "via_offset.bin", 512);
    let source_offset = futures::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(
        payload.clone(),
    ))]);
    let cancel_offset = CancellationToken::new();
    let writer_offset: Writer<std::io::Error> =
        Writer::with_offset(source_offset, res_offset.clone(), cancel_offset, 0);
    let total_offset = run_writer(writer_offset).await.unwrap();

    assert_eq!(total_new, total_offset);
    assert_eq!(total_new, 256);

    // Both resources should have identical data at offset 0.
    let mut buf_new = vec![0u8; 256];
    let mut buf_offset = vec![0u8; 256];
    res_new.read_at(0, &mut buf_new).unwrap();
    res_offset.read_at(0, &mut buf_offset).unwrap();
    assert_eq!(buf_new, buf_offset);
    assert_eq!(buf_new, payload);
}

// 4. Offset overflow returns WriterError::OffsetOverflow

#[tokio::test]
async fn writer_with_offset_overflow_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    // Use a large initial_len so the storage doesn't reject the huge offset write itself.
    // The overflow happens in the Writer arithmetic, not in storage.
    let res = open_resource(&dir, "overflow.bin", 1024);

    // start_offset = u64::MAX - 5. First chunk is 10 bytes.
    // The write_at(u64::MAX - 5, &[..10]) may or may not succeed in storage,
    // but the offset arithmetic: (u64::MAX - 5).checked_add(10) = None => OffsetOverflow.
    let start_offset: u64 = u64::MAX - 5;
    let source = futures::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(vec![
        0u8;
        10
    ]))]);

    let cancel = CancellationToken::new();
    let mut writer: Writer<std::io::Error> =
        Writer::with_offset(source, res.clone(), cancel, start_offset);

    let mut found_overflow = false;
    while let Some(result) = writer.next().await {
        match result {
            // The write_at may fail first (storage error for huge offset),
            // or the checked_add fails after a successful write.
            // Either way we should get an error, not a successful completion.
            Err(WriterError::OffsetOverflow) => {
                found_overflow = true;
                break;
            }
            Err(WriterError::SinkWrite(_)) => {
                // Storage rejected the huge offset — also acceptable.
                // The important thing is we don't get a silent success.
                return;
            }
            Ok(WriterItem::ChunkWritten { .. }) => {
                // Write succeeded but the next poll should fail on overflow.
                // Continue polling.
            }
            Ok(WriterItem::StreamEnded { .. }) => {
                panic!("stream should not end successfully when offset overflows");
            }
            Err(other) => {
                panic!("unexpected error variant: {other:?}");
            }
        }
    }

    // If storage didn't reject the write, we must have seen OffsetOverflow.
    if !found_overflow {
        panic!("expected WriterError::OffsetOverflow but writer completed without error");
    }
}

// 5. Cancellation mid-write terminates cleanly

#[tokio::test]
async fn writer_with_offset_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let res = open_resource(&dir, "cancel.bin", 4096);

    let start_offset: u64 = 500;

    // Use a channel-based stream so we can control when chunks arrive.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let source = tokio_stream::wrappers::ReceiverStream::new(rx);

    let cancel = CancellationToken::new();
    let mut writer: Writer<std::io::Error> =
        Writer::with_offset(source, res.clone(), cancel.clone(), start_offset);

    // Send one chunk so the writer makes progress.
    tx.send(Ok(Bytes::from(vec![0xDDu8; 128]))).await.unwrap();

    // Consume the first ChunkWritten.
    let first = writer.next().await.unwrap().unwrap();
    assert!(
        matches!(
            first,
            WriterItem::ChunkWritten {
                offset: 500,
                len: 128
            }
        ),
        "first item should be ChunkWritten at offset 500, got: {first:?}"
    );

    // Cancel while the writer is waiting for the next chunk.
    cancel.cancel();

    // The writer should terminate — next poll returns None (stream ends).
    let remaining: Vec<_> = writer.collect().await;
    assert!(
        remaining.is_empty(),
        "writer should yield no more items after cancellation, got: {remaining:?}"
    );
}
