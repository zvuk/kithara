//! Audio worker loop and command processing.

use std::{sync::Arc, time::Duration};

use kithara_decode::PcmChunk;
use kithara_stream::{Fetch, Timeline};
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Consumer, Producer},
};
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
    /// Called by the worker loop when `timeline().is_flushing()` or
    /// `timeline().is_seek_pending()` is true. Reads target/epoch from
    /// Timeline, performs the seek on the decoder, then calls
    /// `timeline().clear_seek_pending(epoch)` on success.
    ///
    /// Returns `true` if the seek was applied (or abandoned after max retries),
    /// `false` if it will be retried on the next iteration.
    fn apply_pending_seek(&mut self) -> bool;
}

const BACKOFF_BUSY: Duration = Duration::from_micros(100);
const BACKOFF_EOF: Duration = Duration::from_millis(100);

enum WorkerControl {
    Continue,
    Stop,
}

fn drain_commands<S: AudioWorkerSource>(source: &mut S, cmd_rx: &mut HeapCons<S::Command>) -> bool {
    let mut dropped = 0usize;
    let mut latest = None;
    while let Some(cmd) = cmd_rx.try_pop() {
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
    cmd_rx: &mut HeapCons<S::Command>,
    data_tx: &mut HeapProd<Fetch<S::Chunk>>,
    cancel: &CancellationToken,
    fetch: Fetch<S::Chunk>,
) -> Result<bool, ()> {
    let mut item = fetch;
    loop {
        if cancel.is_cancelled() {
            trace!("audio worker cancelled during backpressure");
            return Err(());
        }

        // Drop chunk and return to main loop if a seek is pending.
        if source.timeline().is_flushing() {
            return Ok(false);
        }

        match data_tx.try_push(item) {
            Ok(()) => return Ok(true),
            Err(returned) => {
                item = returned;
                let mut latest_cmd = None;
                while let Some(cmd) = cmd_rx.try_pop() {
                    latest_cmd = Some(cmd);
                }
                if let Some(cmd) = latest_cmd {
                    source.handle_command(cmd);
                    return Ok(false);
                }
                if cancel.is_cancelled() {
                    trace!("audio worker cancelled during backpressure");
                    return Err(());
                }
                if source.timeline().is_flushing() {
                    return Ok(false);
                }
                kithara_platform::thread::sleep(BACKOFF_BUSY);
            }
        }
    }
}

fn apply_pending_seek_if_needed<S: AudioWorkerSource>(source: &mut S, at_eof: &mut bool) -> bool {
    if !source.timeline().is_flushing() && !source.timeline().is_seek_pending() {
        return false;
    }
    source.apply_pending_seek();
    *at_eof = false;
    true
}

fn handle_eof_idle<S: AudioWorkerSource>(
    source: &mut S,
    cmd_rx: &mut HeapCons<S::Command>,
    cancel: &CancellationToken,
    at_eof: &mut bool,
) -> WorkerControl {
    if apply_pending_seek_if_needed(source, at_eof) {
        return WorkerControl::Continue;
    }

    if let Some(cmd) = cmd_rx.try_pop() {
        source.handle_command(cmd);
        *at_eof = false;
        return WorkerControl::Continue;
    }

    if cancel.is_cancelled() {
        trace!("audio worker cancelled at EOF");
        return WorkerControl::Stop;
    }
    kithara_platform::thread::sleep(BACKOFF_EOF);
    WorkerControl::Continue
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
    reason = "main worker loop with multiple control paths"
)]
pub(super) fn run_audio_loop<S: AudioWorkerSource>(
    mut source: S,
    mut cmd_rx: HeapCons<S::Command>,
    mut data_tx: HeapProd<Fetch<S::Chunk>>,
    preload_notify: &Arc<Notify>,
    preload_chunks: usize,
    cancel: &CancellationToken,
) {
    trace!("audio worker started");
    let mut at_eof = false;
    let mut preloaded = false;
    let mut chunks_sent = 0usize;
    kithara_platform::hang_watchdog! {
        thread: "audio.worker";
        loop {
            if cancel.is_cancelled() {
                trace!("audio worker cancelled");
                return;
            }

            if apply_pending_seek_if_needed(&mut source, &mut at_eof) {
                hang_reset!();
                continue;
            }

            // Apply pending commands eagerly.
            if drain_commands(&mut source, &mut cmd_rx) {
                at_eof = false;
                hang_reset!();
            }

            if at_eof {
                if let WorkerControl::Stop =
                    handle_eof_idle(&mut source, &mut cmd_rx, cancel, &mut at_eof)
                {
                    return;
                }
                continue;
            }

            hang_tick!();
            kithara_platform::thread::yield_now();
            debug!(chunks_sent, "worker: fetching next chunk");
            let fetch = source.fetch_next();
            let is_eof = fetch.is_eof();
            debug!(is_eof, chunks_sent, "worker: fetch_next returned");

            match send_with_backpressure(&mut source, &mut cmd_rx, &mut data_tx, cancel, fetch) {
                Ok(true) => {
                    hang_reset!();
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
                    debug!("worker: cancelled, stopping");
                    trace!("audio worker stopped (cancelled)");
                    return;
                }
            }

            if is_eof {
                mark_eof(&mut preloaded, preload_notify, &mut at_eof);
            }
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
