//! Integration tests for kithara-worker.
//!
//! Tests the full worker implementation with async and sync workers.

use async_trait::async_trait;
use kithara_worker::{
    AsyncWorker, AsyncWorkerSource, EpochItem, Fetch, SimpleItem, SyncWorker, SyncWorkerSource,
    Worker,
};
use tokio::sync::mpsc;

// ==== Async worker tests ====

struct TestAsyncSource {
    data: Vec<i32>,
    pos: usize,
    epoch: u64,
}

impl TestAsyncSource {
    fn new(data: Vec<i32>) -> Self {
        Self {
            data,
            pos: 0,
            epoch: 0,
        }
    }
}

#[async_trait]
impl AsyncWorkerSource for TestAsyncSource {
    type Chunk = i32;
    type Command = usize; // seek position

    async fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        if self.pos >= self.data.len() {
            return Fetch::new(0, true, self.epoch);
        }
        let val = self.data[self.pos];
        self.pos += 1;
        Fetch::new(val, false, self.epoch)
    }

    fn handle_command(&mut self, cmd: Self::Command) -> u64 {
        self.pos = cmd;
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }

    fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[tokio::test]
async fn test_async_worker_basic() {
    let source = TestAsyncSource::new(vec![1, 2, 3]);
    let (cmd_tx, cmd_rx) = mpsc::channel::<usize>(4);
    let (data_tx, data_rx) = kanal::bounded_async::<EpochItem<i32>>(4);

    let worker = AsyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(worker.run());

    // Receive all items
    let item1 = data_rx.recv().await.unwrap();
    assert_eq!(item1.data, 1);
    assert_eq!(item1.epoch, 0);
    assert!(!item1.is_eof);

    let item2 = data_rx.recv().await.unwrap();
    assert_eq!(item2.data, 2);
    assert!(!item2.is_eof);

    let item3 = data_rx.recv().await.unwrap();
    assert_eq!(item3.data, 3);
    assert!(!item3.is_eof);

    // EOF marker after data exhausted
    let eof = data_rx.recv().await.unwrap();
    assert!(eof.is_eof);
    assert_eq!(eof.epoch, 0);

    // Send seek command to restart from position 1
    cmd_tx.send(1).await.unwrap();

    // After seek, worker restarts from position 1
    let item4 = data_rx.recv().await.unwrap();
    assert_eq!(item4.data, 2);
    assert_eq!(item4.epoch, 1); // epoch incremented after seek

    drop(cmd_tx);
}

// ==== Sync worker tests ====

struct TestSyncSource {
    data: Vec<i32>,
    pos: usize,
}

impl TestSyncSource {
    fn new(data: Vec<i32>) -> Self {
        Self { data, pos: 0 }
    }
}

impl SyncWorkerSource for TestSyncSource {
    type Chunk = i32;
    type Command = usize; // seek position

    fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        if self.pos >= self.data.len() {
            return Fetch::new(0, true, 0);
        }
        let val = self.data[self.pos];
        self.pos += 1;
        Fetch::new(val, false, 0)
    }

    fn handle_command(&mut self, cmd: Self::Command) {
        self.pos = cmd;
    }
}

#[tokio::test]
async fn test_sync_worker_basic() {
    let source = TestSyncSource::new(vec![1, 2, 3]);
    let (cmd_tx, cmd_rx) = mpsc::channel::<usize>(4);
    let (data_tx, data_rx) = kanal::bounded::<SimpleItem<i32>>(4);

    let worker = SyncWorker::new(source, cmd_rx, data_tx);
    tokio::spawn(async move { worker.run().await });

    let async_rx = data_rx.as_async();

    // Receive all items
    let item1 = async_rx.recv().await.unwrap();
    assert_eq!(item1.data, 1);
    assert!(!item1.is_eof);

    let item2 = async_rx.recv().await.unwrap();
    assert_eq!(item2.data, 2);
    assert!(!item2.is_eof);

    let item3 = async_rx.recv().await.unwrap();
    assert_eq!(item3.data, 3);
    assert!(!item3.is_eof);

    // EOF marker after data exhausted
    let eof = async_rx.recv().await.unwrap();
    assert!(eof.is_eof);

    // Send command to restart from position 1
    cmd_tx.send(1).await.unwrap();

    // After command, worker restarts from position 1
    let item4 = async_rx.recv().await.unwrap();
    assert_eq!(item4.data, 2);
    assert!(!item4.is_eof);

    drop(cmd_tx);
}
