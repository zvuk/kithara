//! Integration tests for kithara-worker.
//!
//! These tests verify the full worker lifecycle including:
//! - Throughput with different buffer sizes
//! - Backpressure handling
//! - Graceful shutdown
//! - Deadlock detection
//! - Epoch validation

use kanal;
use rstest::*;
use tokio::sync::mpsc;

use kithara_worker::testing::{MockAsyncSource, MockSyncSource, SeekCommand};
use kithara_worker::{AsyncWorker, EpochItem, SimpleItem, SyncWorker, Worker};

/// Test basic AsyncWorker throughput with various buffer sizes.
#[rstest]
#[case(4, 1024, 10)]
#[case(8, 2048, 20)]
#[case(16, 4096, 30)]
#[tokio::test(flavor = "multi_thread")]
async fn test_async_worker_throughput(
    #[case] prefetch: usize,
    #[case] chunk_size: usize,
    #[case] total_chunks: usize,
) {
    let source = MockAsyncSource::new(chunk_size, total_chunks);
    let (cmd_tx, cmd_rx) = mpsc::channel(4);
    let (data_tx, data_rx) = kanal::bounded_async::<EpochItem<Vec<u8>>>(prefetch);

    let worker = AsyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(worker.run());

    // Receive all chunks
    let mut count = 0;
    loop {
        match data_rx.recv().await {
            Ok(item) => {
                if item.is_eof {
                    break;
                }
                count += 1;
                assert_eq!(item.data.len(), chunk_size);
                assert_eq!(item.data[0], count as u8 - 1); // Check pattern
            }
            Err(_) => panic!("Channel closed unexpectedly"),
        }
    }

    assert_eq!(count, total_chunks);
    drop(cmd_tx); // Close command channel
}

/// Test graceful shutdown when command channel is closed.
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(2))]
#[tokio::test(flavor = "multi_thread")]
async fn test_async_worker_shutdown() {
    let source = MockAsyncSource::new(1024, 100);
    let (cmd_tx, cmd_rx) = mpsc::channel(4);
    let (data_tx, data_rx) = kanal::bounded_async(4);

    let worker = AsyncWorker::new(source, cmd_rx, data_tx);
    let handle = tokio::spawn(worker.run());

    // Receive a few items
    for _ in 0..5 {
        let _ = data_rx.recv().await;
    }

    // Close command channel to trigger shutdown
    drop(cmd_tx);

    // Worker should complete
    tokio::time::timeout(std::time::Duration::from_millis(500), handle)
        .await
        .expect("Worker should shutdown gracefully")
        .expect("Worker task should not panic");
}

/// Test backpressure with slow consumer.
///
/// This test ensures that a slow consumer does not cause deadlock,
/// and that the worker properly blocks on send when buffer is full.
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(3))]
#[tokio::test(flavor = "multi_thread")]
async fn test_async_worker_backpressure() {
    let source = MockAsyncSource::new(1024, 20);
    let (cmd_tx, cmd_rx) = mpsc::channel(4);
    let (data_tx, data_rx) = kanal::bounded_async(2); // Small buffer for backpressure

    let worker = AsyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(worker.run());

    // Slow consumer
    let mut count = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        match data_rx.recv().await {
            Ok(item) => {
                if item.is_eof {
                    break;
                }
                count += 1;
            }
            Err(_) => break,
        }
    }

    assert_eq!(count, 20);
    drop(cmd_tx);
}

/// Test epoch invalidation on seek.
///
/// After a seek command, old items should be discarded and new items
/// should have the updated epoch.
#[tokio::test(flavor = "multi_thread")]
async fn test_async_worker_epoch_validation() {
    let source = MockAsyncSource::new(1024, 100);
    let (cmd_tx, cmd_rx) = mpsc::channel(4);
    let (data_tx, data_rx) = kanal::bounded_async(8);

    let worker = AsyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(worker.run());

    // Receive first few items with epoch 0
    for i in 0..3 {
        let item = data_rx.recv().await.unwrap();
        assert_eq!(item.epoch, 0);
        assert_eq!(item.data[0], i as u8);
    }

    // Send seek command with new epoch
    cmd_tx
        .send(SeekCommand {
            offset: 50,
            epoch: 1,
        })
        .await
        .unwrap();

    // Next items should have epoch 1 and start from offset 50
    let item = data_rx.recv().await.unwrap();
    assert_eq!(item.epoch, 1);
    assert_eq!(item.data[0], 50);

    drop(cmd_tx);
}

/// Test SyncWorker basic functionality.
#[rstest]
#[case(4, 512, 5)]
#[case(8, 1024, 10)]
#[tokio::test(flavor = "multi_thread")]
async fn test_sync_worker_basic(
    #[case] prefetch: usize,
    #[case] chunk_size: usize,
    #[case] count: usize,
) {
    let source = MockSyncSource::with_count(chunk_size, count);
    let (cmd_tx, cmd_rx) = mpsc::channel(4);
    let (data_tx, data_rx) = kanal::bounded::<SimpleItem<Vec<u8>>>(prefetch);

    let worker = SyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(worker.run());

    // Receive all chunks
    let async_rx = data_rx.to_async();
    let mut received = 0;
    loop {
        match async_rx.recv().await {
            Ok(item) => {
                if item.is_eof {
                    break;
                }
                received += 1;
                assert_eq!(item.data.len(), chunk_size);
            }
            Err(_) => break,
        }
    }

    assert_eq!(received, count);
    drop(cmd_tx);
}

/// Test SyncWorker graceful shutdown.
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(2))]
#[tokio::test(flavor = "multi_thread")]
async fn test_sync_worker_shutdown() {
    let source = MockSyncSource::with_count(1024, 100);
    let (cmd_tx, cmd_rx) = mpsc::channel(4);
    let (data_tx, data_rx) = kanal::bounded(4);

    let worker = SyncWorker::new(source, cmd_rx, data_tx);
    let handle = tokio::spawn(worker.run());

    // Receive a few items
    let async_rx = data_rx.to_async();
    for _ in 0..5 {
        let _ = async_rx.recv().await;
    }

    // Close command channel
    drop(cmd_tx);

    // Worker should complete
    tokio::time::timeout(std::time::Duration::from_millis(500), handle)
        .await
        .expect("Worker should shutdown gracefully")
        .expect("Worker task should not panic");
}

/// Test that both workers can be spawned concurrently without issues.
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(3))]
#[tokio::test(flavor = "multi_thread")]
async fn test_multiple_workers_concurrent() {
    // Spawn AsyncWorker
    let async_source = MockAsyncSource::new(1024, 10);
    let (async_cmd_tx, async_cmd_rx) = mpsc::channel(4);
    let (async_data_tx, async_data_rx) = kanal::bounded_async(4);
    let async_worker = AsyncWorker::new(async_source, async_cmd_rx, async_data_tx);
    let async_handle = tokio::spawn(async_worker.run());

    // Spawn SyncWorker
    let sync_source = MockSyncSource::with_count(512, 10);
    let (sync_cmd_tx, sync_cmd_rx) = mpsc::channel(4);
    let (sync_data_tx, sync_data_rx) = kanal::bounded(4);
    let sync_worker = SyncWorker::new(sync_source, sync_cmd_rx, sync_data_tx);
    let sync_handle = tokio::spawn(sync_worker.run());

    // Consume from both
    let async_task = tokio::spawn(async move {
        let mut count = 0;
        loop {
            match async_data_rx.recv().await {
                Ok(item) if item.is_eof => break,
                Ok(_) => count += 1,
                Err(_) => break,
            }
        }
        count
    });

    let sync_task = tokio::spawn(async move {
        let async_rx = sync_data_rx.to_async();
        let mut count = 0;
        loop {
            match async_rx.recv().await {
                Ok(item) if item.is_eof => break,
                Ok(_) => count += 1,
                Err(_) => break,
            }
        }
        count
    });

    let async_count = async_task.await.unwrap();
    let sync_count = sync_task.await.unwrap();

    assert_eq!(async_count, 10);
    assert_eq!(sync_count, 10);

    drop(async_cmd_tx);
    drop(sync_cmd_tx);

    async_handle.await.unwrap();
    sync_handle.await.unwrap();
}
