use std::io::{self, Error as IoError, ErrorKind, Read};

use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
use kithara_test_utils::kithara;

use super::{
    Stream, StreamPending, StreamReadError, StreamReadOutcome, StreamType, VariantChangeError,
};
use crate::{NotReadyCause, PendingReason, ReadOutcome, Source, SourceError, StreamError, consts};

/// Per-probe wait policy threaded into [`Stream::try_read_with`]. Internal
/// plumbing, NOT a public knob — it selects the `Source::wait_range` timeout
/// from the caller's statically-known context.
#[derive(Clone, Copy)]
pub(super) enum WaitMode {
    /// RT / cooperative-yield probe (`probe_read`): bounded
    /// `Some(WAIT_RANGE_TIMEOUT)`, returns without blocking.
    Probe,
    /// Off-RT consumer (`impl Read`): `None` — the source parks event-driven
    /// until the range resolves (no wall-clock poll at this layer).
    Block,
}

impl WaitMode {
    const fn timeout(self, probe: Duration) -> Option<Duration> {
        match self {
            Self::Probe => Some(probe),
            Self::Block => None,
        }
    }
}

impl<T: StreamType> Stream<T> {
    /// Per-probe hint passed to [`Source::wait_range`] on the RT worker read
    /// path (`probe_read` → [`WaitMode::Probe`]). Non-blocking-pull sources
    /// (HLS) ignore the value and answer with a single readiness probe; the
    /// backoff between probes lives in the audio scheduler's `Waiting` park
    /// (10ms). The off-RT [`Read`] path passes `None` ([`WaitMode::Block`])
    /// instead and parks event-driven until the range resolves.
    const WAIT_RANGE_TIMEOUT: Duration = Duration::from_millis(10);

    /// Typed read — returns a [`StreamReadOutcome`] discriminating
    /// progress (`Bytes` with [`std::num::NonZeroUsize`]) from non-progress
    /// (`Pending` with a typed [`PendingReason`]) and natural EOF.
    /// `impl Read for Stream` wraps this outcome for `std::io::Read`
    /// consumers.
    ///
    /// # Errors
    ///
    /// Returns [`StreamReadError::Source`] only when the underlying source
    /// reports a genuine I/O failure. Backpressure and a
    /// variant fence are non-errors — they surface as `Ok(Pending(..))`.
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<StreamReadOutcome, StreamReadError> {
        self.try_read_with(buf, WaitMode::Probe)
    }

    /// Awaits only the unit holding the read cursor, never a wider range, since segmented readiness
    /// is all-or-nothing and a slow tail would otherwise block a read the resident head could
    /// already satisfy. If the resource is evicted between `wait_range` and `read_at`, it
    /// re-acquires immediately without parking.
    #[kithara::measure]
    #[kithara::hang_watchdog]
    pub(super) fn try_read_with(
        &mut self,
        buf: &mut [u8],
        wait: WaitMode,
    ) -> Result<StreamReadOutcome, StreamReadError> {
        if buf.is_empty() {
            return Ok(StreamReadOutcome::Eof {
                byte_position: self.source.position(),
            });
        }

        loop {
            let pos = self.source.position();
            let requested_end = pos.saturating_add(buf.len() as u64);
            let unit_end = if self.source.peer_wake().is_some() {
                self.source.byte_map().and_then(|map| {
                    let init = map.init_segment_range();
                    if init.contains(&pos) {
                        Some(init.end)
                    } else {
                        map.segment_at_byte(pos)
                            .map(|segment| segment.byte_range.end)
                    }
                })
            } else {
                None
            };
            let range = pos..unit_end.map_or(requested_end, |end| end.min(requested_end));
            let read_len = usize::try_from(range.end.saturating_sub(pos))
                .map_or(buf.len(), |len| len.min(buf.len()));
            let buf = &mut buf[..read_len];

            let wait_result = self
                .source
                .wait_range(range, wait.timeout(Self::WAIT_RANGE_TIMEOUT));
            let wait_outcome = match wait_result {
                Ok(outcome) => outcome,
                Err(StreamError::Source(SourceError::WaitBudgetExceeded)) => {
                    return Ok(StreamReadOutcome::Pending(PendingReason::NotReady(
                        NotReadyCause::WaitBudgetExhausted,
                    )));
                }
                Err(StreamError::Source(SourceError::Io(error))) => {
                    if let Some(reason) = error
                        .get_ref()
                        .and_then(|inner| inner.downcast_ref::<PendingReason>())
                    {
                        return Ok(StreamReadOutcome::Pending(*reason));
                    }
                    return Err(StreamReadError::Source(error));
                }
                Err(e) => {
                    return Err(StreamReadError::Source(IoError::other(e.to_string())));
                }
            };
            match wait_outcome {
                WaitOutcome::Ready => {}
                WaitOutcome::Eof => {
                    return Ok(StreamReadOutcome::Eof { byte_position: pos });
                }
                WaitOutcome::Interrupted => {
                    return Ok(StreamReadOutcome::Pending(PendingReason::NotReady(
                        NotReadyCause::WaitInterrupted,
                    )));
                }
            }

            match self
                .source
                .read_at(pos, buf)
                .map_err(|e| StreamReadError::Source(IoError::other(e.to_string())))?
            {
                ReadOutcome::Bytes(count) => {
                    hang_reset!();
                    self.source.advance(count.get() as u64);
                    let new_pos = self.source.position();
                    return Ok(StreamReadOutcome::Bytes {
                        count,
                        byte_position: new_pos,
                    });
                }
                ReadOutcome::Eof => {
                    return Ok(StreamReadOutcome::Eof { byte_position: pos });
                }
                ReadOutcome::Pending(PendingReason::Retry) if matches!(wait, WaitMode::Probe) => {
                    return Ok(StreamReadOutcome::Pending(PendingReason::Retry));
                }
                ReadOutcome::Pending(PendingReason::Retry) => {
                    hang_tick!();
                    continue;
                }
                ReadOutcome::Pending(reason) => {
                    return Ok(StreamReadOutcome::Pending(reason));
                }
            }
        }
    }
}

impl<T: StreamType> Stream<T> {
    /// Worker (produce-core) peer wake: a reader-blocked probe arms the wake
    /// lock-free. The audio scheduler shell flushes it off the forbid path, so
    /// the cross-thread `notify_one` (a `kevent`) never fires on the RT core.
    /// No-op for sources without a peer (file streams).
    fn arm_peer_wake(&self) {
        if let Some(wake) = self.source.peer_wake() {
            wake.arm();
        }
    }

    /// Consumer (off-core) peer wake: a not-ready probe wakes the peer
    /// immediately — the consumer never runs on the RT produce core, so the
    /// `notify_one` is allowed and the read is not stalled on the worker's next
    /// pass. No-op for sources without a peer (file streams).
    fn notify_peer_wake(&self) {
        if let Some(wake) = self.source.peer_wake() {
            wake.notify_now();
        }
    }

    /// Map one non-blocking [`Self::try_read`] probe to `std::io::Read` for the worker to park.
    /// Direct consumers use blocking [`Read::read`]; retired sessions return immediately without
    /// waking an obsolete peer, leaving rebuild and peer arming to the new owner.
    ///
    /// # Errors
    /// Propagates source errors; `Interrupted` carries [`StreamPending`] or `SessionRetired`,
    /// and `Other` carries [`VariantChangeError`] at a variant boundary.
    pub fn probe_read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.try_read(buf) {
            Ok(StreamReadOutcome::Bytes { count, .. }) => Ok(count.get()),
            Ok(StreamReadOutcome::Eof { .. }) => Ok(0),
            Ok(StreamReadOutcome::Pending(
                reason @ (PendingReason::NotReady(_) | PendingReason::Retry),
            )) => {
                self.arm_peer_wake();
                Err(IoError::new(
                    ErrorKind::Interrupted,
                    self.snapshot_pending(reason, buf.len()),
                ))
            }
            Ok(StreamReadOutcome::Pending(PendingReason::VariantChange)) => {
                Err(IoError::other(VariantChangeError))
            }
            Ok(StreamReadOutcome::Pending(reason @ PendingReason::SessionRetired)) => {
                Err(IoError::new(ErrorKind::Interrupted, reason))
            }
            Err(StreamReadError::Source(e)) => Err(e),
        }
    }
}

impl<T: StreamType> Read for Stream<T> {
    /// Blocks off the real-time path until bytes, EOF, a seek, a variant change, or an error.
    /// Timeout and stall policy remain owned by the source.
    ///
    /// On an evicted `Retry` range, wakes the peer to trigger a re-fetch and re-loops, so the next
    /// attempt parks in the event-driven `wait_range`. Each re-aim starts a fresh source wait, so
    /// only this loop can see that nothing arrives: returning is its only progress.
    /// Retired sessions return without waiting or peer wake: nobody owns that session's bytes,
    /// and only a rebuilt owner can arm the replacement peer.
    #[kithara::flash(true)]
    #[kithara::hang_watchdog(timeout = consts::READ_HANG_TIMEOUT)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.try_read_with(buf, WaitMode::Block) {
                Ok(StreamReadOutcome::Bytes { count, .. }) => return Ok(count.get()),
                Ok(StreamReadOutcome::Eof { .. }) => return Ok(0),
                Ok(StreamReadOutcome::Pending(
                    PendingReason::NotReady(_) | PendingReason::Retry,
                )) => {
                    hang_tick!();
                    self.notify_peer_wake();
                }
                Ok(StreamReadOutcome::Pending(PendingReason::VariantChange)) => {
                    return Err(IoError::other(VariantChangeError));
                }
                Ok(StreamReadOutcome::Pending(reason @ PendingReason::SessionRetired)) => {
                    return Err(IoError::new(ErrorKind::Interrupted, reason));
                }
                Err(StreamReadError::Source(e)) => return Err(e),
            }
        }
    }
}

impl<T: StreamType> Stream<T> {
    /// Build a typed [`StreamPending`] payload for a
    /// `Pending(NotReady|Retry)` surfaced through `impl Read`. Pulls
    /// live source/timeline state at the moment of the wrap so the
    /// resulting `io::Error` carries the real reason ("data not ready
    /// (`wait_budget_exhausted`): pos=N len=M phase=…")
    /// instead of a bare "data not ready". Decoders downcast on
    /// `StreamPending` to recover the typed [`PendingReason`].
    fn snapshot_pending(&self, reason: PendingReason, want: usize) -> StreamPending {
        let pos = self.source.position();
        let len = self.source.len();
        let phase = self.source.phase_at(pos..pos.saturating_add(want as u64));
        StreamPending {
            len,
            reason,
            phase,
            pos,
            want,
        }
    }
}
