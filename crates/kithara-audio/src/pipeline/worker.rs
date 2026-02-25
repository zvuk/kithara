//! Audio worker loop and command processing.

use std::{sync::Arc, time::Duration};

use kithara_decode::PcmChunk;
use kithara_platform::{Receiver, Sender};
use kithara_stream::{Fetch, Timeline};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};

use crate::traits::AudioEffect;

/// Command for audio worker (non-seek commands only).
///
/// Seek flows entirely through [`Timeline`] atomics.
#[derive(Debug)]
pub(crate) enum AudioCommand {
    // Seek removed — flows through Timeline::initiate_seek / complete_seek.
}

/// Trait for audio sources processed in a blocking worker thread.
pub(crate) trait AudioWorkerSource: Send + 'static {
    type Chunk: Send + 'static;
    type Command: Send + 'static;

    fn fetch_next(&mut self) -> Fetch<Self::Chunk>;
    fn handle_command(&mut self, cmd: Self::Command);

    /// Access the shared timeline for flushing checks.
    fn timeline(&self) -> &Timeline;

    /// Apply a pending seek read from the Timeline.
    ///
    /// Called by the worker loop when `timeline().is_flushing()` is true.
    /// Reads target/epoch from Timeline, performs the seek on the decoder,
    /// then calls `timeline().complete_seek(epoch)`.
    fn apply_pending_seek(&mut self);
}

const BACKOFF_BUSY: Duration = Duration::from_micros(100);
const BACKOFF_EOF: Duration = Duration::from_millis(100);

enum WorkerControl {
    Continue,
    Stop,
}

fn drain_commands<S: AudioWorkerSource>(source: &mut S, cmd_rx: &Receiver<S::Command>) -> bool {
    let mut dropped = 0usize;
    let mut latest = None;
    while let Ok(Some(cmd)) = cmd_rx.try_recv() {
        if latest.is_some() {
            dropped += 1;
        }
        latest = Some(cmd);
    }
    if dropped > 0 {
        trace!(dropped, "audio worker coalesced pending commands");
    }
    if let Some(cmd) = latest {
        source.handle_command(cmd);
        return true;
    }
    false
}

fn send_with_backpressure<S: AudioWorkerSource>(
    source: &mut S,
    cmd_rx: &Receiver<S::Command>,
    data_tx: &Sender<Fetch<S::Chunk>>,
    cancel: &CancellationToken,
    fetch: Fetch<S::Chunk>,
) -> Result<bool, ()> {
    let mut item = Some(fetch);
    loop {
        if cancel.is_cancelled() {
            trace!("audio worker cancelled during backpressure");
            return Err(());
        }

        // Drop chunk and return to main loop if a seek is pending.
        if source.timeline().is_flushing() {
            return Ok(false);
        }

        match data_tx.try_send_option(&mut item) {
            Ok(true) => return Ok(true), // sent
            Ok(false) => {
                // Removed: debug!("worker: channel full, backpressure spin");
                let mut latest_cmd = None;
                loop {
                    match cmd_rx.try_recv() {
                        Ok(Some(cmd)) => latest_cmd = Some(cmd),
                        Ok(None) => break,
                        Err(_) => return Err(()), // channel closed
                    }
                }
                if let Some(cmd) = latest_cmd {
                    source.handle_command(cmd);
                    return Ok(false); // command handled, refetch
                }
                if cancel.is_cancelled() {
                    trace!("audio worker cancelled during backpressure");
                    return Err(());
                }
                // Also check flushing during backpressure spin.
                if source.timeline().is_flushing() {
                    return Ok(false);
                }
                kithara_platform::thread::sleep(BACKOFF_BUSY); // back off and retry
            }
            Err(_) => return Err(()), // channel closed
        }
    }
}

fn apply_pending_seek_if_flushing<S: AudioWorkerSource>(source: &mut S, at_eof: &mut bool) -> bool {
    if !source.timeline().is_flushing() {
        return false;
    }
    source.apply_pending_seek();
    *at_eof = false;
    true
}

fn handle_eof_idle<S: AudioWorkerSource>(
    source: &mut S,
    cmd_rx: &Receiver<S::Command>,
    cancel: &CancellationToken,
    at_eof: &mut bool,
) -> WorkerControl {
    if apply_pending_seek_if_flushing(source, at_eof) {
        return WorkerControl::Continue;
    }

    match cmd_rx.try_recv() {
        Ok(Some(cmd)) => {
            source.handle_command(cmd);
            *at_eof = false;
            WorkerControl::Continue
        }
        Ok(None) => {
            if cancel.is_cancelled() {
                trace!("audio worker cancelled at EOF");
                return WorkerControl::Stop;
            }
            kithara_platform::thread::sleep(BACKOFF_EOF);
            WorkerControl::Continue
        }
        Err(_) => {
            trace!("audio worker stopped (cmd channel closed)");
            WorkerControl::Stop
        }
    }
}

fn mark_preload_progress(
    preloaded: &mut bool,
    chunks_sent: &mut usize,
    preload_chunks: usize,
    preload_notify: &Notify,
) {
    if *preloaded {
        return;
    }
    *chunks_sent += 1;
    if *chunks_sent >= preload_chunks {
        preload_notify.notify_one();
        *preloaded = true;
    }
}

fn mark_eof(preloaded: &mut bool, preload_notify: &Notify, at_eof: &mut bool) {
    if !*preloaded {
        preload_notify.notify_one();
        *preloaded = true;
    }
    *at_eof = true;
}

/// Run a blocking audio loop: drain commands, fetch data, send through channel.
///
/// `preload_chunks` controls how many chunks must be sent before `preload_notify`
/// fires. This ensures the channel has enough data for non-blocking reads.
///
/// `cancel` token signals graceful shutdown when Audio is dropped.
#[expect(
    clippy::cognitive_complexity,
    reason = "diagnostic debug!() temporarily raises complexity"
)]
pub(super) fn run_audio_loop<S: AudioWorkerSource>(
    mut source: S,
    cmd_rx: &Receiver<S::Command>,
    data_tx: &Sender<Fetch<S::Chunk>>,
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

        if apply_pending_seek_if_flushing(&mut source, &mut at_eof) {
            continue;
        }

        // Apply pending commands eagerly.
        if drain_commands(&mut source, cmd_rx) {
            at_eof = false;
        }

        if at_eof {
            if let WorkerControl::Stop = handle_eof_idle(&mut source, cmd_rx, cancel, &mut at_eof) {
                return;
            }
            continue;
        }

        debug!(chunks_sent, "worker: fetching next chunk");
        let fetch = source.fetch_next();
        let is_eof = fetch.is_eof();
        debug!(is_eof, chunks_sent, "worker: fetch_next returned");

        match send_with_backpressure(&mut source, cmd_rx, data_tx, cancel, fetch) {
            Ok(true) => {
                mark_preload_progress(
                    &mut preloaded,
                    &mut chunks_sent,
                    preload_chunks,
                    preload_notify,
                );
            }
            Ok(false) => {
                debug!("worker: backpressure or command, refetching");
                continue;
            }
            Err(()) => {
                debug!("worker: data channel closed, stopping");
                trace!("audio worker stopped (data channel closed)");
                return;
            }
        }

        if is_eof {
            mark_eof(&mut preloaded, preload_notify, &mut at_eof);
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
