//! Async worker implementation for non-blocking sources.

use async_trait::async_trait;
use tracing::trace;

use crate::{
    item::Fetch,
    result::{WorkerResult, WorkerResultExt},
    traits::{AsyncWorkerSource, Worker},
};

/// Generic async worker that runs in a background task.
///
/// The worker continuously fetches data from the source and sends it through
/// a channel. It handles commands (like seek) and tracks epochs for invalidation.
pub struct AsyncWorker<S: AsyncWorkerSource> {
    source: S,
    cmd_rx: kanal::AsyncReceiver<S::Command>,
    data_tx: kanal::AsyncSender<Fetch<S::Chunk>>,
}

impl<S: AsyncWorkerSource> AsyncWorker<S> {
    /// Create a new async worker.
    pub fn new(
        source: S,
        cmd_rx: kanal::AsyncReceiver<S::Command>,
        data_tx: kanal::AsyncSender<Fetch<S::Chunk>>,
    ) -> Self {
        Self {
            source,
            cmd_rx,
            data_tx,
        }
    }

    /// Drain all pending commands, keeping only the last one (command coalescing).
    ///
    /// When multiple commands arrive rapidly (e.g., fast seek operations),
    /// only the final command is processed. This reduces unnecessary work
    /// and improves responsiveness.
    fn drain_pending_commands(&mut self) {
        let mut last_cmd: Option<S::Command> = None;
        let mut coalesced_count = 0u32;

        while let Ok(Some(cmd)) = self.cmd_rx.try_recv() {
            last_cmd = Some(cmd);
            coalesced_count += 1;
        }

        if let Some(cmd) = last_cmd {
            let new_epoch = self.source.handle_command(cmd);
            if coalesced_count > 1 {
                trace!(
                    epoch = new_epoch,
                    coalesced = coalesced_count,
                    "AsyncWorker: coalesced multiple commands"
                );
            } else {
                trace!(epoch = new_epoch, "AsyncWorker: drained pending command");
            }
        }
    }

    /// Send an item through the channel, interruptible by commands.
    /// Returns `WorkerResult::COMMAND_RECEIVED` if command was received and item was discarded.
    /// Returns `WorkerResult::EOF` if the item was EOF.
    async fn send_item(&mut self, fetch: Fetch<S::Chunk>) -> WorkerResult {
        let fetch_epoch = fetch.epoch();
        let is_eof = fetch.is_eof();

        tokio::select! {
            biased;
            cmd_result = self.cmd_rx.recv() => {
                // Command received while waiting to send - handle it
                // The current item is discarded (will be re-fetched with new position)
                match cmd_result {
                    Ok(cmd) => {
                        let new_epoch = self.source.handle_command(cmd);
                        trace!(old_epoch = fetch_epoch, new_epoch, "AsyncWorker: command during send, discarding chunk");
                        WorkerResult::COMMAND_RECEIVED
                    }
                    Err(_) => WorkerResult::STOP, // Channel closed
                }
            }
            result = self.data_tx.send(fetch) => {
                if result.is_ok() {
                    if is_eof {
                        WorkerResult::EOF
                    } else {
                        WorkerResult::CONTINUE
                    }
                } else {
                    WorkerResult::STOP
                }
            }
        }
    }

    /// Run the async worker loop.
    async fn run_worker(mut self) {
        trace!("AsyncWorker started");

        loop {
            // Drain any pending commands first
            self.drain_pending_commands();

            tokio::select! {
                biased;

                // Always prioritize commands
                cmd_result = self.cmd_rx.recv() => {
                    match cmd_result {
                        Ok(cmd) => {
                            let new_epoch = self.source.handle_command(cmd);
                            trace!(epoch = new_epoch, "AsyncWorker: received command");
                            // Continue loop to refetch with new position
                        }
                        Err(_) => break, // Channel closed
                    }
                }

                // Fetch next chunk
                fetch = self.source.fetch_next() => {
                    // Check if epoch changed while we were fetching
                    // This can happen if a command arrived between drain and fetch
                    if fetch.epoch() != self.source.epoch() {
                        trace!(
                            fetch_epoch = fetch.epoch(),
                            current_epoch = self.source.epoch(),
                            "AsyncWorker: epoch mismatch, discarding stale fetch"
                        );
                        continue; // Discard and refetch with new epoch
                    }

                    if fetch.is_eof() {
                        trace!(epoch = fetch.epoch(), "AsyncWorker: EOF reached");
                    }

                    let result = self.send_item(fetch).await;

                    // Check priority: stop > eof > command_received > continue
                    if result.is_stop() {
                        break;
                    }

                    if result.is_eof() {
                        // EOF reached, enter EOF-waiting mode
                        trace!("AsyncWorker: entering EOF-waiting mode");
                        #[allow(clippy::never_loop)]
                        loop {
                            match self.cmd_rx.recv().await {
                                Ok(cmd) => {
                                    let new_epoch = self.source.handle_command(cmd);
                                    trace!(epoch = new_epoch, "AsyncWorker: command at EOF, resuming");
                                    break; // Exit EOF loop, return to main loop
                                }
                                Err(_) => {
                                    trace!("AsyncWorker: command channel closed at EOF");
                                    return; // Exit worker
                                }
                            }
                        }
                    }

                    // is_command_received() and is_continue() are handled implicitly
                    // (just continue the loop)
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
