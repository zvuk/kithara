//! Audio worker loop and command processing.

use std::{sync::Arc, time::Duration};

use kithara_decode::PcmChunk;
use kithara_stream::Fetch;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::trace;

use crate::traits::AudioEffect;

/// Command for audio worker.
#[derive(Debug)]
pub enum AudioCommand {
    /// Seek to position with new epoch.
    Seek { position: Duration, epoch: u64 },
}

/// Trait for audio sources processed in a blocking worker thread.
pub(super) trait AudioWorkerSource: Send + 'static {
    type Chunk: Send + 'static;
    type Command: Send + 'static;

    fn fetch_next(&mut self) -> Fetch<Self::Chunk>;
    fn handle_command(&mut self, cmd: Self::Command);
}

const BACKOFF_BUSY: Duration = Duration::from_micros(100);
const BACKOFF_EOF: Duration = Duration::from_millis(100);

fn drain_commands<S: AudioWorkerSource>(
    source: &mut S,
    cmd_rx: &kanal::Receiver<S::Command>,
) -> bool {
    let mut handled = false;
    while let Ok(Some(cmd)) = cmd_rx.try_recv() {
        source.handle_command(cmd);
        handled = true;
    }
    handled
}

fn send_with_backpressure<S: AudioWorkerSource>(
    source: &mut S,
    cmd_rx: &kanal::Receiver<S::Command>,
    data_tx: &kanal::Sender<Fetch<S::Chunk>>,
    cancel: &CancellationToken,
    fetch: Fetch<S::Chunk>,
) -> Result<bool, ()> {
    let mut item = Some(fetch);
    loop {
        if cancel.is_cancelled() {
            trace!("audio worker cancelled during backpressure");
            return Err(());
        }

        match data_tx.try_send_option(&mut item) {
            Ok(true) => return Ok(true), // sent
            Ok(false) => match cmd_rx.try_recv() {
                Ok(Some(cmd)) => {
                    source.handle_command(cmd);
                    return Ok(false); // command handled, refetch
                }
                Ok(None) => {
                    if cancel.is_cancelled() {
                        trace!("audio worker cancelled during backpressure");
                        return Err(());
                    }
                    std::thread::sleep(BACKOFF_BUSY); // back off and retry
                }
                Err(_) => return Err(()), // channel closed
            },
            Err(_) => return Err(()), // channel closed
        }
    }
}

/// Run a blocking audio loop: drain commands, fetch data, send through channel.
///
/// `preload_chunks` controls how many chunks must be sent before `preload_notify`
/// fires. This ensures the channel has enough data for non-blocking reads.
///
/// `cancel` token signals graceful shutdown when Audio is dropped.
pub(super) fn run_audio_loop<S: AudioWorkerSource>(
    mut source: S,
    cmd_rx: &kanal::Receiver<S::Command>,
    data_tx: &kanal::Sender<Fetch<S::Chunk>>,
    preload_notify: &Arc<Notify>,
    preload_chunks: usize,
    cancel: &CancellationToken,
) {
    trace!("audio worker started");
    let mut at_eof = false;
    let mut preloaded = false;
    let mut chunks_sent = 0usize;

    loop {
        if cancel.is_cancelled() {
            trace!("audio worker cancelled");
            return;
        }

        // Apply pending commands eagerly.
        if drain_commands(&mut source, cmd_rx) {
            at_eof = false;
        }

        if at_eof {
            // Idle until a new command or cancellation.
            match cmd_rx.try_recv() {
                Ok(Some(cmd)) => {
                    source.handle_command(cmd);
                    at_eof = false;
                }
                Ok(None) => {
                    if cancel.is_cancelled() {
                        trace!("audio worker cancelled at EOF");
                        return;
                    }
                    std::thread::sleep(BACKOFF_EOF);
                }
                Err(_) => {
                    trace!("audio worker stopped (cmd channel closed)");
                    return;
                }
            }
            continue;
        }

        let fetch = source.fetch_next();
        let is_eof = fetch.is_eof();

        match send_with_backpressure(&mut source, cmd_rx, data_tx, cancel, fetch) {
            Ok(true) => {
                if !preloaded {
                    chunks_sent += 1;
                    if chunks_sent >= preload_chunks {
                        preload_notify.notify_one();
                        preloaded = true;
                    }
                }
            }
            Ok(false) => {
                // Command handled during backpressure: refetch next loop.
                continue;
            }
            Err(()) => {
                trace!("audio worker stopped (data channel closed)");
                return;
            }
        }

        if is_eof {
            if !preloaded {
                // short stream — unblock readers even if we didn't hit preload_chunks
                preload_notify.notify_one();
                preloaded = true;
            }
            at_eof = true;
        }
    }
}

/// Apply effects chain to a chunk.
pub(super) fn apply_effects(
    effects: &mut [Box<dyn AudioEffect>],
    mut chunk: PcmChunk,
) -> Option<PcmChunk> {
    for effect in &mut *effects {
        chunk = effect.process(chunk)?;
    }
    Some(chunk)
}

/// Flush effects chain at end of stream.
pub(super) fn flush_effects(effects: &mut [Box<dyn AudioEffect>]) -> Option<PcmChunk> {
    let mut chunk: Option<PcmChunk> = None;
    for effect in &mut *effects {
        chunk = match chunk.take() {
            Some(input) => effect.process(input),
            None => effect.flush(),
        };
    }
    chunk
}

/// Reset effects chain (e.g. after seek).
pub(super) fn reset_effects(effects: &mut [Box<dyn AudioEffect>]) {
    for effect in &mut *effects {
        effect.reset();
    }
}
