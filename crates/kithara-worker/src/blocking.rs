//! Sync worker implementation for blocking sources.

use async_trait::async_trait;
use tracing::trace;

use crate::{
    item::Fetch,
    traits::{SyncWorkerSource, Worker},
};

/// Blocking worker for synchronous sources.
///
/// This is the blocking counterpart to `AsyncWorker`. It runs in a
/// `spawn_blocking` task and uses `Fetch` with epoch always set to 0.
///
/// ## Usage
///
/// ```ignore
/// let source = MySyncSource::new();
/// let (cmd_tx, cmd_rx) = kanal::bounded(4);
/// let (data_tx, data_rx) = kanal::bounded(4);
///
/// let worker = SyncWorker::new(source, cmd_rx, data_tx);
/// tokio::task::spawn_blocking(move || worker.run());
/// ```
pub struct SyncWorker<S: SyncWorkerSource> {
    source: S,
    cmd_rx: kanal::Receiver<S::Command>,
    data_tx: kanal::Sender<Fetch<S::Chunk>>,
}

impl<S: SyncWorkerSource> SyncWorker<S> {
    /// Create a new sync worker.
    pub fn new(
        source: S,
        cmd_rx: kanal::Receiver<S::Command>,
        data_tx: kanal::Sender<Fetch<S::Chunk>>,
    ) -> Self {
        Self {
            source,
            cmd_rx,
            data_tx,
        }
    }

    /// Run the blocking worker loop.
    ///
    /// Call this from a dedicated thread or `spawn_blocking` task.
    pub fn run_blocking(mut self) {
        trace!("SyncWorker started");

        let mut at_eof = false;

        loop {
            // Drain all pending commands
            let mut cmd_received = false;
            while let Ok(Some(cmd)) = self.cmd_rx.try_recv() {
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
                            // Channel full, check if command channel is closed
                            match self.cmd_rx.try_recv() {
                                Ok(Some(cmd)) => {
                                    // Command received, handle it and discard current item
                                    self.source.handle_command(cmd);
                                    trace!("SyncWorker: command during send, discarding chunk");
                                    return; // Exit to refetch with new position
                                }
                                Ok(None) => {
                                    // No command available, wait and retry send
                                    std::thread::sleep(std::time::Duration::from_micros(100));
                                }
                                Err(_) => {
                                    // Command channel closed, exit gracefully
                                    trace!("SyncWorker: command channel closed during send, shutting down");
                                    return;
                                }
                            }
                        }
                        Err(_) => {
                            return; // Data channel closed
                        }
                    }
                }

                if is_eof {
                    at_eof = true;
                }
            } else {
                // At EOF, periodically check for commands with timeout
                // This prevents deadlock in spawn_blocking thread
                match self.cmd_rx.try_recv() {
                    Ok(Some(cmd)) => {
                        self.source.handle_command(cmd);
                        at_eof = false;
                        trace!("SyncWorker: received command at EOF, resuming");
                    }
                    Ok(None) => {
                        // No command within check, loop again
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                    Err(_) => {
                        // Channel closed, exit gracefully
                        trace!("SyncWorker: command channel closed at EOF, shutting down");
                        break;
                    }
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
        tokio::task::spawn_blocking(move || self.run_blocking())
            .await
            .ok();
    }
}
