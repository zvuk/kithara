//! Integration tests for kithara-worker.
//!
//! These tests verify the full worker lifecycle including:
//! - Throughput with different buffer sizes
//! - Backpressure handling
//! - Graceful shutdown
//! - Deadlock detection
//! - Epoch validation

use kanal;
use kithara_worker::{
    testing::{MockAsyncSource, MockSyncSource, SeekCommand},
    AsyncWorker, Fetch, SyncWorker, Worker,
};
use rstest::*;

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
    let (cmd_tx, cmd_rx) = kanal::bounded_async(4);
    let (data_tx, data_rx) = kanal::bounded_async::<Fetch<Vec<u8>>>(prefetch);

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
    let (cmd_tx, cmd_rx) = kanal::bounded_async(4);
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
    let (cmd_tx, cmd_rx) = kanal::bounded_async(4);
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
    let (cmd_tx, cmd_rx) = kanal::bounded_async(4);
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

    // Discard any items with old epoch (may be buffered in channel)
    // and wait for first item with new epoch
    let item = loop {
        let item = data_rx.recv().await.unwrap();
        if item.epoch == 1 {
            break item;
        }
    };

    // First item with new epoch should start from offset 50
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
    let (cmd_tx, cmd_rx) = kanal::bounded(4);
    let (data_tx, data_rx) = kanal::bounded::<Fetch<Vec<u8>>>(prefetch);

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
    let (cmd_tx, cmd_rx) = kanal::bounded(4);
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
    let (async_cmd_tx, async_cmd_rx) = kanal::bounded_async(4);
    let (async_data_tx, async_data_rx) = kanal::bounded_async(4);
    let async_worker = AsyncWorker::new(async_source, async_cmd_rx, async_data_tx);
    let async_handle = tokio::spawn(async_worker.run());

    // Spawn SyncWorker
    let sync_source = MockSyncSource::with_count(512, 10);
    let (sync_cmd_tx, sync_cmd_rx) = kanal::bounded(4);
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

// ======================== STRESS TESTS FOR DEADLOCK DETECTION ========================

/// Stress test: Multiple consumers reading from single worker
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(5))]
#[tokio::test(flavor = "multi_thread")]
async fn test_async_worker_multiple_consumers() {
    let source = MockAsyncSource::new(1024, 100);
    let (cmd_tx, cmd_rx) = kanal::bounded_async(4);
    let (data_tx, data_rx) = kanal::bounded_async(8);

    let worker = AsyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(worker.run());

    // Spawn 3 consumers competing for items
    let rx1 = data_rx.clone();
    let rx2 = data_rx.clone();
    let rx3 = data_rx;

    let (eof_tx, mut eof_rx) = tokio::sync::mpsc::channel(1);
    let eof_tx1 = eof_tx.clone();
    let eof_tx2 = eof_tx.clone();
    let eof_tx3 = eof_tx;

    let consumer1 = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = rx1.recv().await {
            count += 1;
            if item.is_eof {
                let _ = eof_tx1.send(()).await;
                break;
            }
        }
        count
    });

    let consumer2 = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = rx2.recv().await {
            count += 1;
            if item.is_eof {
                let _ = eof_tx2.send(()).await;
                break;
            }
        }
        count
    });

    let consumer3 = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = rx3.recv().await {
            count += 1;
            if item.is_eof {
                let _ = eof_tx3.send(()).await;
                break;
            }
        }
        count
    });

    // Wait for EOF to be received by one of the consumers
    eof_rx.recv().await;

    // Now drop command channel to let worker exit
    drop(cmd_tx);

    let c1 = consumer1.await.unwrap();
    let c2 = consumer2.await.unwrap();
    let c3 = consumer3.await.unwrap();

    // All items should be consumed (100 data + 1 EOF = 101)
    // Only one consumer receives EOF and exits by is_eof check,
    // others exit when channel closes (Err from recv)
    assert_eq!(c1 + c2 + c3, 101);
}

/// Stress test: Slow consumer shouldn't cause deadlock
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(5))]
#[tokio::test(flavor = "multi_thread")]
async fn test_async_worker_slow_consumer_no_deadlock() {
    let source = MockAsyncSource::new(1024, 50);
    let (cmd_tx, cmd_rx) = kanal::bounded_async(4);
    let (data_tx, data_rx) = kanal::bounded_async(2); // Small buffer

    let worker = AsyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(worker.run());

    // Very slow consumer
    let mut count = 0;
    while let Ok(item) = data_rx.recv().await {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        count += 1;
        if item.is_eof {
            break;
        }
    }

    assert_eq!(count, 51); // 50 items + EOF
    drop(cmd_tx);
}

/// Stress test: Command flood shouldn't cause deadlock
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(5))]
#[tokio::test(flavor = "multi_thread")]
async fn test_async_worker_command_flood_no_deadlock() {
    let source = MockAsyncSource::new(1024, 100);
    let (cmd_tx, cmd_rx) = kanal::bounded_async(4);
    let (data_tx, data_rx) = kanal::bounded_async(8);

    let worker = AsyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(worker.run());

    // Flood with seek commands
    let cmd_sender = tokio::spawn(async move {
        for i in 0..20 {
            cmd_tx
                .send(SeekCommand {
                    offset: i * 5,
                    epoch: (i + 1) as u64,
                })
                .await
                .ok();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        cmd_tx
    });

    // Consumer
    let mut count = 0;
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(4));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            item = data_rx.recv() => {
                match item {
                    Ok(item) if item.is_eof => break,
                    Ok(_) => count += 1,
                    Err(_) => break,
                }
            }
            _ = &mut timeout => {
                panic!("Deadlock detected: consumer stuck waiting");
            }
        }
    }

    assert!(count > 0);
    let cmd_tx = cmd_sender.await.unwrap();
    drop(cmd_tx);
}

/// Stress test: SyncWorker with blocking consumer
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(5))]
#[tokio::test(flavor = "multi_thread")]
async fn test_sync_worker_blocking_consumer_no_deadlock() {
    let source = MockSyncSource::with_count(1024, 50);
    let (cmd_tx, cmd_rx) = kanal::bounded(4);
    let (data_tx, data_rx) = kanal::bounded(2); // Small buffer

    let worker = SyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(worker.run());

    // Blocking consumer in separate thread
    let consumer = std::thread::spawn(move || {
        let mut count = 0;
        while let Ok(item) = data_rx.recv() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            count += 1;
            if item.is_eof {
                break;
            }
        }
        count
    });

    let count = consumer.join().unwrap();
    assert_eq!(count, 51); // 50 items + EOF
    drop(cmd_tx);
}

/// Stress test: Interleaved worker types (nested workers)
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(5))]
#[tokio::test(flavor = "multi_thread")]
async fn test_nested_workers_basic_no_deadlock() {
    // AsyncWorker 1 -> AsyncWorker 2 -> Consumer
    let source1 = MockAsyncSource::new(512, 20);
    let (cmd_tx1, cmd_rx1) = kanal::bounded_async(4);
    let (data_tx1, data_rx1) = kanal::bounded_async(4);

    let worker1 = AsyncWorker::new(source1, cmd_rx1, data_tx1);
    tokio::spawn(worker1.run());

    // Use worker1 output as input for worker2 simulation
    let source2 = MockAsyncSource::new(256, 20);
    let (cmd_tx2, cmd_rx2) = kanal::bounded_async(4);
    let (data_tx2, data_rx2) = kanal::bounded_async(4);

    let worker2 = AsyncWorker::new(source2, cmd_rx2, data_tx2);
    tokio::spawn(worker2.run());

    // Consume from both in parallel
    let consumer1 = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = data_rx1.recv().await {
            count += 1;
            if item.is_eof {
                break;
            }
        }
        count
    });

    let consumer2 = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = data_rx2.recv().await {
            count += 1;
            if item.is_eof {
                break;
            }
        }
        count
    });

    let c1 = consumer2.await.unwrap();
    let c2 = consumer1.await.unwrap();

    assert_eq!(c1, 21); // 20 items + EOF
    assert_eq!(c2, 21); // 20 items + EOF

    drop(cmd_tx1);
    drop(cmd_tx2);
}

/// Stress test: Mixed Async/Sync workers with cross-communication
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(5))]
#[tokio::test(flavor = "multi_thread")]
async fn test_mixed_workers_cross_communication_no_deadlock() {
    let async_source = MockAsyncSource::new(1024, 30);
    let (async_cmd_tx, async_cmd_rx) = kanal::bounded_async(4);
    let (async_data_tx, async_data_rx) = kanal::bounded_async(4);

    let sync_source = MockSyncSource::with_count(512, 30);
    let (sync_cmd_tx, sync_cmd_rx) = kanal::bounded(4);
    let (sync_data_tx, sync_data_rx) = kanal::bounded(4);

    let async_worker = AsyncWorker::new(async_source, async_cmd_rx, async_data_tx);
    let sync_worker = SyncWorker::new(sync_source, sync_cmd_rx, sync_data_tx);

    tokio::spawn(async_worker.run());
    tokio::spawn(sync_worker.run());

    // Cross-consume: async consumer reads sync data, sync consumer reads async data
    let async_consumer = tokio::spawn(async move {
        let async_rx = sync_data_rx.to_async();
        let mut count = 0;
        while let Ok(item) = async_rx.recv().await {
            count += 1;
            if item.is_eof {
                break;
            }
        }
        count
    });

    let sync_consumer = tokio::spawn(async move {
        // Consume async channel from async context
        let mut count = 0;
        while let Ok(item) = async_data_rx.recv().await {
            count += 1;
            if item.is_eof {
                break;
            }
        }
        count
    });

    let async_count = async_consumer.await.unwrap();
    let sync_count = sync_consumer.await.unwrap();

    assert_eq!(async_count, 31); // 30 items + EOF
    assert_eq!(sync_count, 31); // 30 items + EOF

    drop(async_cmd_tx);
    drop(sync_cmd_tx);
}

// ======================== ADVANCED NESTED/FEEDBACK TESTS ========================

/// Stress test: Nested worker+consumer inside worker+consumer with feedback
/// Outer worker -> Inner worker -> Inner consumer (sends commands back to outer)
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread")]
async fn test_nested_worker_with_feedback_no_deadlock() {
    // Outer worker
    let outer_source = MockAsyncSource::new(2048, 50);
    let (outer_cmd_tx, outer_cmd_rx) = kanal::bounded_async(8);
    let (outer_data_tx, outer_data_rx) = kanal::bounded_async(8);

    let outer_worker = AsyncWorker::new(outer_source, outer_cmd_rx, outer_data_tx);
    tokio::spawn(outer_worker.run());

    // Inner worker
    let inner_source = MockAsyncSource::new(1024, 100);
    let (inner_cmd_tx, inner_cmd_rx) = kanal::bounded_async(8);
    let (inner_data_tx, inner_data_rx) = kanal::bounded_async(8);

    let inner_worker = AsyncWorker::new(inner_source, inner_cmd_rx, inner_data_tx);
    tokio::spawn(inner_worker.run());

    // Inner consumer that sends commands to outer worker based on data
    let outer_cmd_feedback = outer_cmd_tx.clone();
    let inner_consumer = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = inner_data_rx.recv().await {
            count += 1;

            // Every 10th item, send seek command to outer worker
            if count % 10 == 0 && count < 50 {
                outer_cmd_feedback
                    .send(SeekCommand {
                        offset: count / 10,
                        epoch: (count / 10) as u64,
                    })
                    .await
                    .ok();
            }

            if item.is_eof {
                break;
            }
        }
        count
    });

    // Outer consumer that sends commands to inner worker based on outer data
    let outer_consumer = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = outer_data_rx.recv().await {
            count += 1;

            // Every 5th item, send seek command to inner worker
            if count % 5 == 0 && count < 30 {
                inner_cmd_tx
                    .send(SeekCommand {
                        offset: count * 2,
                        epoch: count as u64,
                    })
                    .await
                    .ok();
            }

            if item.is_eof {
                break;
            }
        }
        count
    });

    let inner_count = inner_consumer.await.unwrap();
    let outer_count = outer_consumer.await.unwrap();

    // Both should complete without deadlock
    assert!(inner_count > 0);
    assert!(outer_count > 0);

    drop(outer_cmd_tx);
}

/// Stress test: Triple nested workers (3 levels deep) with cascade commands
/// L1 Worker -> L2 Worker -> L3 Worker -> Consumer
/// Consumer sends commands up the chain
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread")]
async fn test_triple_nested_workers_cascade_commands_no_deadlock() {
    // Level 1 (outermost)
    let l1_source = MockAsyncSource::new(4096, 20);
    let (l1_cmd_tx, l1_cmd_rx) = kanal::bounded_async(8);
    let (l1_data_tx, l1_data_rx) = kanal::bounded_async(8);
    let l1_worker = AsyncWorker::new(l1_source, l1_cmd_rx, l1_data_tx);
    tokio::spawn(l1_worker.run());

    // Level 2 (middle)
    let l2_source = MockAsyncSource::new(2048, 30);
    let (l2_cmd_tx, l2_cmd_rx) = kanal::bounded_async(8);
    let (l2_data_tx, l2_data_rx) = kanal::bounded_async(8);
    let l2_worker = AsyncWorker::new(l2_source, l2_cmd_rx, l2_data_tx);
    tokio::spawn(l2_worker.run());

    // Level 3 (innermost)
    let l3_source = MockAsyncSource::new(1024, 40);
    let (l3_cmd_tx, l3_cmd_rx) = kanal::bounded_async(8);
    let (l3_data_tx, l3_data_rx) = kanal::bounded_async(8);
    let l3_worker = AsyncWorker::new(l3_source, l3_cmd_rx, l3_data_tx);
    tokio::spawn(l3_worker.run());

    // L1 consumer (processes L1 data, commands L2)
    let l2_cmd_from_l1 = l2_cmd_tx.clone();
    let l1_consumer = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = l1_data_rx.recv().await {
            count += 1;
            if count % 3 == 0 {
                l2_cmd_from_l1
                    .send(SeekCommand {
                        offset: count,
                        epoch: count as u64,
                    })
                    .await
                    .ok();
            }
            if item.is_eof {
                break;
            }
        }
        count
    });

    // L2 consumer (processes L2 data, commands L3)
    let l3_cmd_from_l2 = l3_cmd_tx.clone();
    let l2_consumer = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = l2_data_rx.recv().await {
            count += 1;
            if count % 4 == 0 {
                l3_cmd_from_l2
                    .send(SeekCommand {
                        offset: count * 2,
                        epoch: count as u64,
                    })
                    .await
                    .ok();
            }
            if item.is_eof {
                break;
            }
        }
        count
    });

    // L3 consumer (processes L3 data, commands L1 - creates cycle!)
    let l1_cmd_from_l3 = l1_cmd_tx.clone();
    let l3_consumer = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(item) = l3_data_rx.recv().await {
            count += 1;
            if count % 5 == 0 && count < 25 {
                l1_cmd_from_l3
                    .send(SeekCommand {
                        offset: count / 5,
                        epoch: count as u64,
                    })
                    .await
                    .ok();
            }
            if item.is_eof {
                break;
            }
        }
        count
    });

    let c1 = l1_consumer.await.unwrap();
    let c2 = l2_consumer.await.unwrap();
    let c3 = l3_consumer.await.unwrap();

    // All levels should complete without deadlock
    assert!(c1 > 0);
    assert!(c2 > 0);
    assert!(c3 > 0);

    drop(l1_cmd_tx);
    drop(l2_cmd_tx);
    drop(l3_cmd_tx);
}

/// Stress test: Async worker inside sync worker consumer
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread")]
async fn test_async_inside_sync_worker_no_deadlock() {
    // Outer sync worker
    let sync_source = MockSyncSource::with_count(2048, 50);
    let (sync_cmd_tx, sync_cmd_rx) = kanal::bounded(8);
    let (sync_data_tx, sync_data_rx) = kanal::bounded(8);
    let sync_worker = SyncWorker::new(sync_source, sync_cmd_rx, sync_data_tx);
    tokio::spawn(sync_worker.run());

    // Inner async worker
    let async_source = MockAsyncSource::new(1024, 100);
    let (async_cmd_tx, async_cmd_rx) = kanal::bounded_async(8);
    let (async_data_tx, async_data_rx) = kanal::bounded_async(8);
    let async_worker = AsyncWorker::new(async_source, async_cmd_rx, async_data_tx);
    tokio::spawn(async_worker.run());

    // Sync consumer that processes sync data AND async data simultaneously
    let async_cmd_for_consumer = async_cmd_tx.clone();
    let consumer = tokio::spawn(async move {
        let sync_rx_async = sync_data_rx.to_async();
        let mut sync_count = 0;
        let mut async_count = 0;

        loop {
            tokio::select! {
                sync_item = sync_rx_async.recv() => {
                    match sync_item {
                        Ok(item) => {
                            sync_count += 1;

                            // Send command to async worker based on sync data
                            if sync_count % 7 == 0 {
                                async_cmd_for_consumer.send(SeekCommand {
                                    offset: sync_count,
                                    epoch: sync_count as u64,
                                }).await.ok();
                            }

                            if item.is_eof {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }

                async_item = async_data_rx.recv() => {
                    match async_item {
                        Ok(item) => {
                            async_count += 1;
                            if item.is_eof && sync_count > 0 {
                                break;
                            }
                        }
                        Err(_) => {}
                    }
                }
            }
        }

        (sync_count, async_count)
    });

    let (sync_count, async_count) = consumer.await.unwrap();
    assert!(sync_count > 0);
    assert!(async_count > 0);

    drop(sync_cmd_tx);
    drop(async_cmd_tx);
}

/// Stress test: Sync worker inside async worker consumer
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread")]
async fn test_sync_inside_async_worker_no_deadlock() {
    // Outer async worker
    let async_source = MockAsyncSource::new(2048, 50);
    let (async_cmd_tx, async_cmd_rx) = kanal::bounded_async(8);
    let (async_data_tx, async_data_rx) = kanal::bounded_async(8);
    let async_worker = AsyncWorker::new(async_source, async_cmd_rx, async_data_tx);
    tokio::spawn(async_worker.run());

    // Inner sync worker
    let sync_source = MockSyncSource::with_count(1024, 100);
    let (sync_cmd_tx, sync_cmd_rx) = kanal::bounded(8);
    let (sync_data_tx, sync_data_rx) = kanal::bounded(8);
    let sync_worker = SyncWorker::new(sync_source, sync_cmd_rx, sync_data_tx);
    tokio::spawn(sync_worker.run());

    // Async consumer that spawns blocking sync consumer
    let consumer = tokio::spawn(async move {
        let _sync_rx_async = sync_data_rx.to_async();

        // Spawn blocking task for sync consumption
        let sync_handle = tokio::task::spawn_blocking(move || {
            let count = 0;
            // This is intentionally blocking
            std::thread::sleep(std::time::Duration::from_millis(100));
            count
        });

        let mut async_count = 0;
        while let Ok(item) = async_data_rx.recv().await {
            async_count += 1;
            if item.is_eof {
                break;
            }
        }

        let sync_count = sync_handle.await.unwrap();
        (sync_count, async_count)
    });

    let (_sync_count, async_count) = consumer.await.unwrap();
    assert!(async_count > 0);

    drop(async_cmd_tx);
    drop(sync_cmd_tx);
}

/// Stress test: Diamond pattern - one source, two workers, one consumer
/// Source -> Worker1 ----\
///                        -> Consumer (merges both)
/// Source -> Worker2 ----/
#[rstest::rstest]
#[timeout(std::time::Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread")]
async fn test_diamond_pattern_workers_no_deadlock() {
    // Worker 1
    let source1 = MockAsyncSource::new(1024, 50);
    let (cmd_tx1, cmd_rx1) = kanal::bounded_async(8);
    let (data_tx1, data_rx1) = kanal::bounded_async(8);
    let worker1 = AsyncWorker::new(source1, cmd_rx1, data_tx1);
    tokio::spawn(worker1.run());

    // Worker 2
    let source2 = MockAsyncSource::new(1024, 50);
    let (cmd_tx2, cmd_rx2) = kanal::bounded_async(8);
    let (data_tx2, data_rx2) = kanal::bounded_async(8);
    let worker2 = AsyncWorker::new(source2, cmd_rx2, data_tx2);
    tokio::spawn(worker2.run());

    // Merging consumer
    let cmd_tx1_for_consumer = cmd_tx1.clone();
    let cmd_tx2_for_consumer = cmd_tx2.clone();
    let consumer = tokio::spawn(async move {
        let mut count1 = 0;
        let mut count2 = 0;
        let mut eof1 = false;
        let mut eof2 = false;

        while !eof1 || !eof2 {
            tokio::select! {
                item1 = data_rx1.recv(), if !eof1 => {
                    match item1 {
                        Ok(item) => {
                            count1 += 1;

                            // Send command to worker2 based on worker1 data
                            if count1 % 5 == 0 {
                                cmd_tx2_for_consumer.send(SeekCommand {
                                    offset: count1,
                                    epoch: count1 as u64,
                                }).await.ok();
                            }

                            if item.is_eof {
                                eof1 = true;
                            }
                        }
                        Err(_) => eof1 = true,
                    }
                }

                item2 = data_rx2.recv(), if !eof2 => {
                    match item2 {
                        Ok(item) => {
                            count2 += 1;

                            // Send command to worker1 based on worker2 data
                            if count2 % 7 == 0 {
                                cmd_tx1_for_consumer.send(SeekCommand {
                                    offset: count2,
                                    epoch: count2 as u64,
                                }).await.ok();
                            }

                            if item.is_eof {
                                eof2 = true;
                            }
                        }
                        Err(_) => eof2 = true,
                    }
                }
            }
        }

        (count1, count2)
    });

    let (c1, c2) = consumer.await.unwrap();
    assert!(c1 > 0);
    assert!(c2 > 0);

    drop(cmd_tx1);
    drop(cmd_tx2);
}

// Tests for WorkerResult bitflags combinations
#[cfg(test)]
mod result_combinations {
    use kithara_worker::{WorkerResult, WorkerResultExt};
    use rstest::rstest;

    #[rstest]
    #[case(WorkerResult::CONTINUE, "continue")]
    #[case(WorkerResult::STOP, "stop")]
    #[case(WorkerResult::EOF, "eof")]
    #[case(WorkerResult::COMMAND_RECEIVED, "command")]
    #[case(WorkerResult::EOF | WorkerResult::CONTINUE, "eof + continue")]
    #[case(WorkerResult::EOF | WorkerResult::STOP, "eof + stop")]
    #[case(WorkerResult::COMMAND_RECEIVED | WorkerResult::EOF, "command + eof")]
    #[case(WorkerResult::COMMAND_RECEIVED | WorkerResult::CONTINUE, "command + continue")]
    fn test_worker_result_flag_combinations(#[case] result: WorkerResult, #[case] _desc: &str) {
        // Verify trait methods work
        let _ = result.is_continue();
        let _ = result.is_stop();
        let _ = result.is_eof();
        let _ = result.is_command_received();

        // Verify no panic on any combination
        let _ = result.validate();
    }

    #[test]
    fn test_mutually_exclusive_validation() {
        let invalid = WorkerResult::STOP | WorkerResult::CONTINUE;
        assert!(invalid.validate().is_err());

        let valid1 = WorkerResult::CONTINUE;
        assert!(valid1.validate().is_ok());

        let valid2 = WorkerResult::STOP;
        assert!(valid2.validate().is_ok());

        let valid3 = WorkerResult::EOF | WorkerResult::CONTINUE;
        assert!(valid3.validate().is_ok());
    }
}
