//! Tests for `Writer::with_offset()` — range-request resume path and `OffsetOverflow` error.

use bytes::Bytes;
use futures::StreamExt;
use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource, ResourceExt};
use kithara_stream::{Writer, WriterError, WriterItem};
use rstest::rstest;
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

#[rstest]
#[case(
    100,
    vec![vec![0xABu8; 512]],
    vec![(100_u64, 512_usize)],
    612_u64
)]
#[case(
    200,
    vec![vec![0xAAu8; 100], vec![0xBBu8; 100], vec![0xCCu8; 100]],
    vec![(200_u64, 100_usize), (300_u64, 100_usize), (400_u64, 100_usize)],
    500_u64
)]
#[tokio::test]
async fn writer_with_offset_writes_expected_offsets(
    #[case] start_offset: u64,
    #[case] chunks: Vec<Vec<u8>>,
    #[case] expected_offsets: Vec<(u64, usize)>,
    #[case] expected_total_bytes: u64,
) {
    let dir = tempfile::tempdir().unwrap();
    let res = open_resource(&dir, "offset_write.bin", 4096);
    let source_chunks = chunks.clone();
    let source = futures::stream::iter(
        source_chunks
            .into_iter()
            .map(|chunk| Ok::<Bytes, std::io::Error>(Bytes::from(chunk))),
    );

    let cancel = CancellationToken::new();
    let mut writer: Writer<std::io::Error> =
        Writer::with_offset(source, res.clone(), cancel, start_offset);

    let mut offsets = Vec::new();
    let mut stream_end_total = None;
    while let Some(result) = writer.next().await {
        match result.unwrap() {
            WriterItem::ChunkWritten { offset, len } => {
                offsets.push((offset, len));
            }
            WriterItem::StreamEnded { total_bytes } => {
                stream_end_total = Some(total_bytes);
            }
        }
    }

    assert_eq!(offsets, expected_offsets);
    assert_eq!(stream_end_total, Some(expected_total_bytes));

    for (chunk, (offset, len)) in chunks.iter().zip(expected_offsets.iter()) {
        let mut buf = vec![0u8; *len];
        let n = res.read_at(*offset, &mut buf).unwrap();
        assert_eq!(n, *len);
        assert_eq!(buf, *chunk);
    }
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
