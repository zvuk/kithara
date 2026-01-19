//! Prefetch worker primitives - re-exports from kithara-worker.
//!
//! This module provides backward compatibility aliases for the generic worker
//! patterns now provided by the `kithara-worker` crate.
//!
//! ## Migration guide
//!
//! Old names (still supported):
//! - `PrefetchSource` → Use `AsyncWorkerSource` from `kithara-worker`
//! - `PrefetchWorker` → Use `AsyncWorker` from `kithara-worker`
//! - `PrefetchedItem<C>` → Use `EpochItem<C>` from `kithara-worker`
//! - `PrefetchConsumer` → Use `EpochConsumer` or `EpochValidator` from `kithara-worker`
//! - `PrefetchResult` → Use `WorkerResult` from `kithara-worker`
//!
//! For blocking/sync sources:
//! - Use `SyncWorkerSource` and `SyncWorker` from `kithara-worker`

#![forbid(unsafe_code)]

// Re-export everything from kithara-worker
pub use kithara_worker::{
    AlwaysValid, AsyncWorker, AsyncWorkerSource, EpochConsumer, EpochItem, EpochValidator,
    ItemValidator, SimpleItem, SyncWorker, SyncWorkerSource, WorkerItem, WorkerResult,
};

// Backward compatibility trait aliases via blanket impls
pub trait PrefetchSource: AsyncWorkerSource {}
impl<T: AsyncWorkerSource> PrefetchSource for T {}

pub trait BlockingSource: SyncWorkerSource {}
impl<T: SyncWorkerSource> BlockingSource for T {}

// Backward compatibility type aliases
pub type PrefetchWorker<S> = AsyncWorker<S>;
pub type PrefetchedItem<C> = EpochItem<C>;
pub type PrefetchConsumer = EpochConsumer;
pub type PrefetchResult = WorkerResult;
pub type BlockingWorker<S> = SyncWorker<S>;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    // Test that backward compatibility aliases work

    struct TestSource {
        data: Vec<i32>,
        pos: usize,
        epoch: u64,
    }

    impl TestSource {
        fn new(data: Vec<i32>) -> Self {
            Self {
                data,
                pos: 0,
                epoch: 0,
            }
        }
    }

    impl AsyncWorkerSource for TestSource {
        type Chunk = i32;
        type Command = usize;

        async fn fetch_next(&mut self) -> Option<Self::Chunk> {
            if self.pos >= self.data.len() {
                return None;
            }
            let val = self.data[self.pos];
            self.pos += 1;
            Some(val)
        }

        fn handle_command(&mut self, cmd: Self::Command) -> u64 {
            self.pos = cmd;
            self.epoch = self.epoch.wrapping_add(1);
            self.epoch
        }

        fn epoch(&self) -> u64 {
            self.epoch
        }

        fn eof_chunk(&self) -> Self::Chunk {
            0
        }
    }

    #[tokio::test]
    async fn test_prefetch_worker_backward_compat() {
        let source = TestSource::new(vec![1, 2, 3]);
        let (cmd_tx, cmd_rx) = mpsc::channel::<usize>(4);
        let (data_tx, data_rx) = kanal::bounded_async::<PrefetchedItem<i32>>(4);

        let worker = PrefetchWorker::new(source, cmd_rx, data_tx);
        tokio::spawn(worker.run());

        let item1 = data_rx.recv().await.unwrap();
        assert_eq!(item1.data, 1);

        drop(cmd_tx);
    }

    #[test]
    fn test_prefetch_consumer_backward_compat() {
        let mut consumer = PrefetchConsumer::new();
        assert_eq!(consumer.epoch, 0);

        let item = PrefetchedItem {
            data: 42,
            epoch: 0,
            is_eof: false,
        };
        assert!(consumer.is_valid(&item));

        consumer.next_epoch();
        assert!(!consumer.is_valid(&item));
    }
}
