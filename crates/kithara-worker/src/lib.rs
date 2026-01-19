//! Generic worker primitives for async and blocking data sources.
//!
//! This crate provides reusable worker patterns for processing data in background tasks:
//!
//! - **`AsyncWorker`**: Async worker for async sources (e.g., HTTP streams)
//!   - Uses `AsyncWorkerSource` trait with async `fetch_next()`
//!   - Produces `EpochItem<C>` with epoch tracking for seek invalidation
//!   - Validated by `EpochValidator`
//!
//! - **`SyncWorker`**: Blocking worker for sync sources (e.g., audio decoders)
//!   - Uses `SyncWorkerSource` trait with sync `fetch_next()`
//!   - Produces `SimpleItem<C>` without epoch overhead
//!   - Validated by `AlwaysValid` (no invalidation)
//!
//! Both workers follow the same protocol: commands via mpsc, data via kanal channels,
//! with backpressure and EOF signaling.

#![forbid(unsafe_code)]

use std::future::Future;

use tokio::sync::mpsc;
use tracing::trace;

// ============================================================================
// Generic worker item abstraction
// ============================================================================

/// Generic trait for worker items.
///
/// Allows different worker types to use different item metadata (e.g., with or without epochs).
pub trait WorkerItem: Send + 'static {
    /// The type of data contained in the item.
    type Data: Send + 'static;

    /// Get a reference to the data.
    fn data(&self) -> &Self::Data;

    /// Consume the item and return the data.
    fn into_data(self) -> Self::Data;

    /// Check if this is an EOF item.
    fn is_eof(&self) -> bool;
}

/// Item with epoch tracking for invalidation on seek.
///
/// Used by `AsyncWorker` for async sources that support seeking.
#[derive(Debug, Clone)]
pub struct EpochItem<C> {
    /// The data chunk.
    pub data: C,
    /// Epoch at which this item was fetched.
    pub epoch: u64,
    /// Whether this is the final item (EOF).
    pub is_eof: bool,
}

impl<C: Send + 'static> WorkerItem for EpochItem<C> {
    type Data = C;

    fn data(&self) -> &C {
        &self.data
    }

    fn into_data(self) -> C {
        self.data
    }

    fn is_eof(&self) -> bool {
        self.is_eof
    }
}

/// Simple item without epoch tracking.
///
/// Used by `SyncWorker` for synchronous sources that don't support seeking.
#[derive(Debug, Clone)]
pub struct SimpleItem<C> {
    /// The data chunk.
    pub data: C,
    /// Whether this is the final item (EOF).
    pub is_eof: bool,
}

impl<C: Send + 'static> WorkerItem for SimpleItem<C> {
    type Data = C;

    fn data(&self) -> &C {
        &self.data
    }

    fn into_data(self) -> C {
        self.data
    }

    fn is_eof(&self) -> bool {
        self.is_eof
    }
}

// ============================================================================
// Item validation abstraction
// ============================================================================

/// Trait for validating worker items.
///
/// Different validators can be used depending on whether epoch tracking is needed.
pub trait ItemValidator<I: WorkerItem> {
    /// Check if an item is valid.
    fn is_valid(&self, item: &I) -> bool;
}

/// Validator that checks epoch for invalidation.
///
/// Used with `EpochItem` to discard outdated items after seek.
#[derive(Debug, Clone)]
pub struct EpochValidator {
    /// Current consumer epoch.
    pub epoch: u64,
}

impl EpochValidator {
    /// Create a new epoch validator.
    pub fn new() -> Self {
        Self { epoch: 0 }
    }

    /// Increment epoch (called on seek).
    pub fn next_epoch(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }
}

impl<C: Send + 'static> ItemValidator<EpochItem<C>> for EpochValidator {
    fn is_valid(&self, item: &EpochItem<C>) -> bool {
        item.epoch == self.epoch
    }
}

impl Default for EpochValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// Validator that accepts all items (no invalidation).
///
/// Used with `SimpleItem` when seeking is not supported.
#[derive(Debug, Clone, Copy)]
pub struct AlwaysValid;

impl<C: Send + 'static> ItemValidator<SimpleItem<C>> for AlwaysValid {
    fn is_valid(&self, _item: &SimpleItem<C>) -> bool {
        true
    }
}

/// Consumer-side helper for processing items with epoch validation.
///
/// This is an alias for `EpochValidator` for backward compatibility.
pub type EpochConsumer = EpochValidator;

// ============================================================================
// Worker result
// ============================================================================

/// Result of a single worker iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerResult {
    /// Continue processing.
    Continue,
    /// End of data reached.
    Eof,
    /// Stop worker (error or channel closed).
    Stop,
}

// ============================================================================
// Async worker
// ============================================================================

/// Trait for async sources that can be processed by a worker.
///
/// Implementors provide:
/// - `Chunk`: The type of data chunks to process (e.g., `Vec<u8>` for bytes, `PcmChunk<T>` for audio)
/// - `Command`: The type of commands (e.g., seek position)
/// - `fetch_next`: Async method to get the next chunk
/// - `handle_command`: Process a command (e.g., update position on seek)
/// - `eof_chunk`: Create a chunk to signal EOF
pub trait AsyncWorkerSource: Send + 'static {
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

/// Generic async worker that runs in a background task.
///
/// The worker continuously fetches data from the source and sends it through
/// a channel. It handles commands (like seek) and tracks epochs for invalidation.
pub struct AsyncWorker<S: AsyncWorkerSource> {
    source: S,
    cmd_rx: mpsc::Receiver<S::Command>,
    data_tx: kanal::AsyncSender<EpochItem<S::Chunk>>,
}

impl<S: AsyncWorkerSource> AsyncWorker<S> {
    /// Create a new async worker.
    pub fn new(
        source: S,
        cmd_rx: mpsc::Receiver<S::Command>,
        data_tx: kanal::AsyncSender<EpochItem<S::Chunk>>,
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
            trace!(epoch = new_epoch, "AsyncWorker: drained pending command");
            processed = true;
        }
        processed
    }

    /// Send an item through the channel, interruptible by commands.
    /// Returns `(result, command_received)` where `command_received` indicates if a seek was processed.
    async fn send_item(&mut self, chunk: S::Chunk, is_eof: bool) -> (WorkerResult, bool) {
        let item = EpochItem {
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
                        trace!(epoch = new_epoch, "AsyncWorker: command during send");
                        (WorkerResult::Continue, true)
                    }
                    None => (WorkerResult::Stop, false), // Channel closed
                }
            }
            result = self.data_tx.send(item) => {
                if result.is_ok() {
                    (WorkerResult::Continue, false)
                } else {
                    (WorkerResult::Stop, false)
                }
            }
        }
    }

    /// Run the async worker loop.
    pub async fn run(mut self) {
        trace!("AsyncWorker started");

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
                            trace!(epoch = new_epoch, "AsyncWorker: received command");
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
                                WorkerResult::Continue => {}
                                WorkerResult::Stop => break,
                                WorkerResult::Eof => unreachable!(),
                            }
                        }
                        None => {
                            trace!(epoch = self.source.epoch(), "AsyncWorker: EOF reached");
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

        trace!("AsyncWorker stopped");
    }
}

// ============================================================================
// Sync worker
// ============================================================================

/// Trait for synchronous sources that can be processed by a blocking worker.
///
/// This is the blocking counterpart to `AsyncWorkerSource`, designed for sources
/// like audio decoders that have synchronous APIs.
pub trait SyncWorkerSource: Send + 'static {
    /// Type of data chunks produced.
    type Chunk: Send + 'static;

    /// Type of commands that can be sent to the source.
    type Command: Send + 'static;

    /// Fetch the next chunk of data (synchronous).
    ///
    /// Returns `Some(chunk)` if data is available, `None` on EOF.
    fn fetch_next(&mut self) -> Option<Self::Chunk>;

    /// Handle a command.
    fn handle_command(&mut self, cmd: Self::Command);

    /// Create a chunk to signal EOF.
    fn eof_chunk(&self) -> Self::Chunk;
}

/// Blocking worker for synchronous sources.
///
/// This is the blocking counterpart to `AsyncWorker`. It runs in a
/// `spawn_blocking` task and uses `SimpleItem` (no epoch tracking).
///
/// ## Usage
///
/// ```ignore
/// let source = MySyncSource::new();
/// let (cmd_tx, cmd_rx) = mpsc::channel(4);
/// let (data_tx, data_rx) = kanal::bounded(4);
///
/// let worker = SyncWorker::new(source, cmd_rx, data_tx);
/// tokio::task::spawn_blocking(move || worker.run());
/// ```
pub struct SyncWorker<S: SyncWorkerSource> {
    source: S,
    cmd_rx: mpsc::Receiver<S::Command>,
    data_tx: kanal::Sender<SimpleItem<S::Chunk>>,
}

impl<S: SyncWorkerSource> SyncWorker<S> {
    /// Create a new sync worker.
    pub fn new(
        source: S,
        cmd_rx: mpsc::Receiver<S::Command>,
        data_tx: kanal::Sender<SimpleItem<S::Chunk>>,
    ) -> Self {
        Self {
            source,
            cmd_rx,
            data_tx,
        }
    }

    /// Run the blocking worker loop.
    ///
    /// This should be called inside a `spawn_blocking` task.
    pub fn run(mut self) {
        trace!("SyncWorker started");

        let mut at_eof = false;

        loop {
            // Drain all pending commands
            let mut cmd_received = false;
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                self.source.handle_command(cmd);
                cmd_received = true;
                trace!("SyncWorker: drained pending command");
            }

            if cmd_received {
                at_eof = false;
            }

            if !at_eof {
                // Fetch next chunk
                match self.source.fetch_next() {
                    Some(chunk) => {
                        let mut item_opt = Some(SimpleItem {
                            data: chunk,
                            is_eof: false,
                        });
                        // Non-blocking send with retry
                        loop {
                            match self.data_tx.try_send_option(&mut item_opt) {
                                Ok(true) => break, // Successfully sent
                                Ok(false) => {
                                    // Channel full, wait and retry
                                    std::thread::sleep(std::time::Duration::from_micros(100));
                                }
                                Err(_) => {
                                    return; // Channel closed
                                }
                            }
                        }
                    }
                    None => {
                        trace!("SyncWorker: EOF reached");
                        let eof_chunk = self.source.eof_chunk();
                        let mut eof_item_opt = Some(SimpleItem {
                            data: eof_chunk,
                            is_eof: true,
                        });
                        let _ = self.data_tx.try_send_option(&mut eof_item_opt);
                        at_eof = true;
                    }
                }
            } else {
                // At EOF, block on commands
                match self.cmd_rx.blocking_recv() {
                    Some(cmd) => {
                        self.source.handle_command(cmd);
                        at_eof = false;
                        trace!("SyncWorker: received command at EOF, resuming");
                    }
                    None => break, // Channel closed
                }
            }
        }

        trace!("SyncWorker stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    impl AsyncWorkerSource for TestAsyncSource {
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

        fn fetch_next(&mut self) -> Option<Self::Chunk> {
            if self.pos >= self.data.len() {
                return None;
            }
            let val = self.data[self.pos];
            self.pos += 1;
            Some(val)
        }

        fn handle_command(&mut self, cmd: Self::Command) {
            self.pos = cmd;
        }

        fn eof_chunk(&self) -> Self::Chunk {
            0 // EOF marker for i32
        }
    }

    #[tokio::test]
    async fn test_sync_worker_basic() {
        let source = TestSyncSource::new(vec![1, 2, 3]);
        let (cmd_tx, cmd_rx) = mpsc::channel::<usize>(4);
        let (data_tx, data_rx) = kanal::bounded::<SimpleItem<i32>>(4);

        let worker = SyncWorker::new(source, cmd_rx, data_tx);
        tokio::task::spawn_blocking(move || worker.run());

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

    // ==== Validator tests ====

    #[test]
    fn test_epoch_validator() {
        let mut validator = EpochValidator::new();
        assert_eq!(validator.epoch, 0);

        let item0 = EpochItem {
            data: 42,
            epoch: 0,
            is_eof: false,
        };
        assert!(validator.is_valid(&item0));

        validator.next_epoch();
        assert!(!validator.is_valid(&item0));

        let item1 = EpochItem {
            data: 43,
            epoch: 1,
            is_eof: false,
        };
        assert!(validator.is_valid(&item1));
    }

    #[test]
    fn test_always_valid_validator() {
        let validator = AlwaysValid;
        let item = SimpleItem {
            data: 42,
            is_eof: false,
        };
        assert!(validator.is_valid(&item));

        let eof_item = SimpleItem {
            data: 0,
            is_eof: true,
        };
        assert!(validator.is_valid(&eof_item));
    }

    // ==== WorkerItem trait tests ====

    #[test]
    fn test_worker_item_trait() {
        let epoch_item = EpochItem {
            data: 42,
            epoch: 5,
            is_eof: false,
        };
        assert_eq!(*epoch_item.data(), 42);
        assert!(!epoch_item.is_eof());

        let simple_item = SimpleItem {
            data: 100,
            is_eof: true,
        };
        assert_eq!(*simple_item.data(), 100);
        assert!(simple_item.is_eof());

        let data = simple_item.into_data();
        assert_eq!(data, 100);
    }
}
