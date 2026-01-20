//! Async worker implementation for non-blocking sources.

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::trace;

use crate::item::Fetch;
use crate::result::WorkerResult;
use crate::traits::{AsyncWorkerSource, Worker};

// Re-export for backwards compatibility
use crate::item::EpochItem;

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
        let item = Fetch::new(chunk, is_eof, self.source.epoch());

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
    async fn run_worker(mut self) {
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
                fetch = self.source.fetch_next(), if !at_eof => {
                    let is_eof = fetch.is_eof();
                    let epoch = fetch.epoch();

                    if is_eof {
                        trace!(epoch, "AsyncWorker: EOF reached");
                    }

                    let (send_result, cmd_received) = self.send_item(fetch.data, is_eof).await;
                    if cmd_received {
                        at_eof = false; // Reset EOF on command
                    } else if is_eof {
                        at_eof = true;
                    }

                    match send_result {
                        WorkerResult::Continue => {}
                        WorkerResult::Stop => break,
                        WorkerResult::Eof => unreachable!(),
                    }
                }
            }
        }

        trace!("AsyncWorker stopped");
    }
}

#[async_trait]
impl<S: AsyncWorkerSource> Worker for AsyncWorker<S> {
    async fn run(self) {
        self.run_worker().await
    }
}
