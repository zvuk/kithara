//! Generic prefetch worker primitives.
//!
//! This module provides a reusable pattern for prefetching data in a background task
//! with epoch-based invalidation on seek. Used by `SyncReader` (bytes) and can be used
//! by audio decoders for sample prefetching.

#![forbid(unsafe_code)]

use std::future::Future;

use tokio::sync::mpsc;
use tracing::trace;

/// Prefetched item with epoch for invalidation tracking.
#[derive(Debug, Clone)]
pub struct PrefetchedItem<C> {
    /// The prefetched data chunk.
    pub data: C,
    /// Epoch at which this item was fetched.
    pub epoch: u64,
    /// Whether this is the final item (EOF).
    pub is_eof: bool,
}

/// Result of a single prefetch iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefetchResult {
    /// Continue prefetching.
    Continue,
    /// End of data reached.
    Eof,
    /// Stop worker (error or channel closed).
    Stop,
}

/// Trait for sources that can be prefetched.
///
/// Implementors provide:
/// - `Chunk`: The type of data chunks to prefetch (e.g., `Vec<u8>` for bytes, `PcmChunk<T>` for audio)
/// - `Command`: The type of commands (e.g., seek position)
/// - `fetch_next`: Async method to get the next chunk
/// - `handle_command`: Process a command (e.g., update position on seek)
/// - `eof_chunk`: Create a chunk to signal EOF
pub trait PrefetchSource: Send + 'static {
    /// Type of data chunks produced.
    type Chunk: Send + 'static;

    /// Type of commands that can be sent to the source.
    type Command: Send + 'static;

    /// Fetch the next chunk of data.
    ///
    /// Returns `Some(chunk)` if data is available, `None` on EOF.
    fn fetch_next(&mut self) -> impl Future<Output = Option<Self::Chunk>> + Send;

    /// Handle a command (e.g., seek).
    ///
    /// Returns the new epoch after handling the command.
    fn handle_command(&mut self, cmd: Self::Command) -> u64;

    /// Get current epoch.
    fn epoch(&self) -> u64;

    /// Create a chunk to signal EOF.
    ///
    /// This is sent through the channel when EOF is reached so consumers
    /// can distinguish between "no data yet" and "end of stream".
    fn eof_chunk(&self) -> Self::Chunk;
}

/// Generic prefetch worker that runs in a background task.
///
/// The worker continuously fetches data from the source and sends it through
/// a channel. It handles commands (like seek) and tracks epochs for invalidation.
pub struct PrefetchWorker<S: PrefetchSource> {
    source: S,
    cmd_rx: mpsc::Receiver<S::Command>,
    data_tx: kanal::AsyncSender<PrefetchedItem<S::Chunk>>,
}

impl<S: PrefetchSource> PrefetchWorker<S> {
    /// Create a new prefetch worker.
    pub fn new(
        source: S,
        cmd_rx: mpsc::Receiver<S::Command>,
        data_tx: kanal::AsyncSender<PrefetchedItem<S::Chunk>>,
    ) -> Self {
        Self {
            source,
            cmd_rx,
            data_tx,
        }
    }

    /// Drain all pending commands. Returns true if any command was processed.
    fn drain_pending_commands(&mut self) -> bool {
        let mut processed = false;
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            let new_epoch = self.source.handle_command(cmd);
            trace!(epoch = new_epoch, "Worker: drained pending command");
            processed = true;
        }
        processed
    }

    /// Send a prefetched item through the channel, interruptible by commands.
    /// Returns `(result, command_received)` where `command_received` indicates if a seek was processed.
    async fn send_item(&mut self, chunk: S::Chunk, is_eof: bool) -> (PrefetchResult, bool) {
        let item = PrefetchedItem {
            data: chunk,
            epoch: self.source.epoch(),
            is_eof,
        };

        tokio::select! {
            biased;
            cmd_opt = self.cmd_rx.recv() => {
                // Command received while waiting to send - handle it
                // The current item is discarded (will be re-fetched with new position)
                match cmd_opt {
                    Some(cmd) => {
                        let new_epoch = self.source.handle_command(cmd);
                        trace!(epoch = new_epoch, "Worker: command during send");
                        (PrefetchResult::Continue, true)
                    }
                    None => (PrefetchResult::Stop, false), // Channel closed
                }
            }
            result = self.data_tx.send(item) => {
                if result.is_ok() {
                    (PrefetchResult::Continue, false)
                } else {
                    (PrefetchResult::Stop, false)
                }
            }
        }
    }

    /// Run the prefetch worker loop.
    pub async fn run(mut self) {
        trace!("PrefetchWorker started");

        let mut at_eof = false;

        loop {
            // Drain any pending commands first (and reset EOF if command received)
            if self.drain_pending_commands() {
                at_eof = false;
            }

            tokio::select! {
                biased;

                // Always prioritize commands
                cmd_opt = self.cmd_rx.recv() => {
                    match cmd_opt {
                        Some(cmd) => {
                            let new_epoch = self.source.handle_command(cmd);
                            trace!(epoch = new_epoch, "Worker: received command");
                            at_eof = false; // Reset EOF state, will refetch
                        }
                        None => break, // Channel closed
                    }
                }

                // Only fetch if not at EOF
                result = self.source.fetch_next(), if !at_eof => {
                    match result {
                        Some(chunk) => {
                            let (send_result, cmd_received) = self.send_item(chunk, false).await;
                            if cmd_received {
                                at_eof = false; // Reset EOF on command
                            }
                            match send_result {
                                PrefetchResult::Continue => {}
                                PrefetchResult::Stop => break,
                                PrefetchResult::Eof => unreachable!(),
                            }
                        }
                        None => {
                            trace!(epoch = self.source.epoch(), "Worker: EOF reached");
                            let eof_chunk = self.source.eof_chunk();
                            let (_, cmd_received) = self.send_item(eof_chunk, true).await;
                            if cmd_received {
                                at_eof = false; // Command received, don't go to EOF
                            } else {
                                at_eof = true;
                            }
                        }
                    }
                }
            }
        }

        trace!("PrefetchWorker stopped");
    }
}

/// Consumer-side helper for processing prefetched items with epoch validation.
#[derive(Debug, Clone)]
pub struct PrefetchConsumer {
    /// Current consumer position/state epoch.
    pub epoch: u64,
}

impl PrefetchConsumer {
    /// Create a new consumer.
    pub fn new() -> Self {
        Self { epoch: 0 }
    }

    /// Check if a prefetched item is valid for current epoch.
    pub fn is_valid<C>(&self, item: &PrefetchedItem<C>) -> bool {
        item.epoch == self.epoch
    }

    /// Increment epoch (called on seek).
    pub fn next_epoch(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }
}

impl Default for PrefetchConsumer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    impl PrefetchSource for TestSource {
        type Chunk = i32;
        type Command = usize; // seek position

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
            0 // EOF marker for i32
        }
    }

    #[tokio::test]
    async fn test_prefetch_worker_basic() {
        let source = TestSource::new(vec![1, 2, 3]);
        let (cmd_tx, cmd_rx) = mpsc::channel::<usize>(4);
        let (data_tx, data_rx) = kanal::bounded_async::<PrefetchedItem<i32>>(4);

        let worker = PrefetchWorker::new(source, cmd_rx, data_tx);
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

    #[test]
    fn test_prefetch_consumer_epoch() {
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

        let new_item = PrefetchedItem {
            data: 43,
            epoch: 1,
            is_eof: false,
        };
        assert!(consumer.is_valid(&new_item));
    }
}
