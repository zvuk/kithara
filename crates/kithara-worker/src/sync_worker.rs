//! Sync worker implementation for blocking sources.

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::trace;

use crate::item::Fetch;
use crate::traits::{SyncWorkerSource, Worker};

// Re-export for backwards compatibility
use crate::item::SimpleItem;

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
    fn run_worker(mut self) {
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
                let fetch = self.source.fetch_next();
                let is_eof = fetch.is_eof();

                if is_eof {
                    trace!("SyncWorker: EOF reached");
                }

                // For sync sources, epoch is always 0
                let mut item_opt = Some(Fetch::new(fetch.data, is_eof, 0));

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

                if is_eof {
                    at_eof = true;
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

#[async_trait]
impl<S: SyncWorkerSource> Worker for SyncWorker<S> {
    async fn run(self) {
        // Run sync worker in a blocking task
        tokio::task::spawn_blocking(move || self.run_worker())
            .await
            .ok();
    }
}
