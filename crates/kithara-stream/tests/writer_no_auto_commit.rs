//! TDD test: Writer stream end should NOT auto-commit resource.
//!
//! Writer yields `WriterItem::StreamEnded { total_bytes }` when the source stream ends.
//! The resource must remain Active — caller decides whether to commit.

use bytes::Bytes;
use futures::StreamExt;
use kithara_storage::{OpenMode, ResourceExt, ResourceStatus, StorageOptions, StorageResource};
use kithara_stream::{Writer, WriterItem};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn writer_stream_end_does_not_commit_resource() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_file.bin");

    let res = StorageResource::open(StorageOptions {
        path,
        initial_len: Some(1024),
        mode: OpenMode::ReadWrite,
        cancel: CancellationToken::new(),
    })
    .unwrap();

    // Create a stream that yields some data then ends
    let data = vec![
        Ok(Bytes::from(vec![0u8; 512])),
        Ok(Bytes::from(vec![1u8; 256])),
    ];
    let source_stream = futures::stream::iter(data);

    let cancel = CancellationToken::new();
    let mut writer: Writer<std::io::Error> = Writer::new(source_stream, res.clone(), cancel);

    // Consume all writer items
    let mut total = 0u64;
    while let Some(result) = writer.next().await {
        match result.unwrap() {
            WriterItem::ChunkWritten { len, .. } => {
                total += len as u64;
            }
            WriterItem::StreamEnded { total_bytes } => {
                assert_eq!(total_bytes, 768); // 512 + 256
                break;
            }
        }
    }

    assert_eq!(total, 768);

    // Resource must NOT be committed — it should still be Active
    let status = res.status();
    assert_eq!(
        status,
        ResourceStatus::Active,
        "Resource should remain Active after stream end, not auto-committed"
    );
}

#[tokio::test]
async fn writer_run_returns_total_on_stream_ended() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_file_run.bin");

    let res = StorageResource::open(StorageOptions {
        path,
        initial_len: Some(1024),
        mode: OpenMode::ReadWrite,
        cancel: CancellationToken::new(),
    })
    .unwrap();

    let data = vec![
        Ok(Bytes::from(vec![0u8; 100])),
        Ok(Bytes::from(vec![1u8; 200])),
    ];
    let source_stream = futures::stream::iter(data);

    let cancel = CancellationToken::new();
    let writer: Writer<std::io::Error> = Writer::new(source_stream, res.clone(), cancel);

    let total = writer.run(|_, _| {}).await.unwrap();
    assert_eq!(total, 300);

    // Resource must still be Active (not committed by Writer)
    let status = res.status();
    assert_eq!(
        status,
        ResourceStatus::Active,
        "Writer::run should not auto-commit resource"
    );
}
