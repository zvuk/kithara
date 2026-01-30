//! Audio worker loop and command processing.

use std::{sync::Arc, time::Duration};

use kithara_decode::PcmChunk;
use kithara_stream::Fetch;
use tokio::sync::Notify;
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

/// Run a blocking audio loop: drain commands, fetch data, send through channel.
///
/// `preload_chunks` controls how many chunks must be sent before `preload_notify`
/// fires. This ensures the channel has enough data for non-blocking reads.
pub(super) fn run_audio_loop<S: AudioWorkerSource>(
    mut source: S,
    cmd_rx: kanal::Receiver<S::Command>,
    data_tx: kanal::Sender<Fetch<S::Chunk>>,
    preload_notify: Arc<Notify>,
    preload_chunks: usize,
) {
    trace!("audio worker started");
    let mut at_eof = false;
    let mut preloaded = false;
    let mut chunks_sent: usize = 0;

    loop {
        // Drain pending commands
        let mut cmd_received = false;
        while let Ok(Some(cmd)) = cmd_rx.try_recv() {
            source.handle_command(cmd);
            cmd_received = true;
        }
        if cmd_received {
            at_eof = false;
        }

        if !at_eof {
            let fetch = source.fetch_next();
            let is_eof = fetch.is_eof();

            let mut item = Some(fetch);

            // Non-blocking send with backpressure
            loop {
                match data_tx.try_send_option(&mut item) {
                    Ok(true) => {
                        if !preloaded {
                            chunks_sent += 1;
                            if chunks_sent >= preload_chunks {
                                preload_notify.notify_one();
                                preloaded = true;
                            }
                        }
                        break;
                    }
                    Ok(false) => match cmd_rx.try_recv() {
                        Ok(Some(cmd)) => {
                            source.handle_command(cmd);
                            break; // Discard stale chunk, refetch
                        }
                        Ok(None) => {
                            std::thread::sleep(std::time::Duration::from_micros(100));
                        }
                        Err(_) => return,
                    },
                    Err(_) => return,
                }
            }

            if is_eof {
                // Signal preload even if we haven't reached preload_chunks
                // (short stream scenario).
                if !preloaded {
                    preload_notify.notify_one();
                    preloaded = true;
                }
                at_eof = true;
            }
        } else {
            match cmd_rx.try_recv() {
                Ok(Some(cmd)) => {
                    source.handle_command(cmd);
                    at_eof = false;
                }
                Ok(None) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    }

    trace!("audio worker stopped");
}

/// Apply effects chain to a chunk.
pub(super) fn apply_effects(
    effects: &mut [Box<dyn AudioEffect>],
    mut chunk: PcmChunk<f32>,
) -> Option<PcmChunk<f32>> {
    for effect in effects.iter_mut() {
        chunk = effect.process(chunk)?;
    }
    Some(chunk)
}

/// Flush effects chain at end of stream.
pub(super) fn flush_effects(effects: &mut [Box<dyn AudioEffect>]) -> Option<PcmChunk<f32>> {
    let mut result: Option<PcmChunk<f32>> = None;
    for effect in effects.iter_mut() {
        result = effect.flush();
    }
    result
}

/// Reset effects chain (e.g. after seek).
pub(super) fn reset_effects(effects: &mut [Box<dyn AudioEffect>]) {
    for effect in effects.iter_mut() {
        effect.reset();
    }
}
