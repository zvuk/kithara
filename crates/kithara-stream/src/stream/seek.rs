use std::{
    io::{self, Error as IoError, ErrorKind, Seek, SeekFrom},
    ops::Range,
};

use kithara_test_utils::kithara;
use tracing::debug;

use super::{Stream, StreamType};
use crate::{PendingReason, Source, SourcePhase};

/// Typed error from [`Stream::seek`] for an absolute byte target that
/// lands beyond the stream's known length.
///
/// Surfaced as the typed payload of an `io::Error` (kind
/// [`ErrorKind::InvalidInput`]) so consumers like Symphonia preserve it
/// through their own error chain. Decoders downcast to recover the
/// structured info and classify the failure as caller-side (the seek
/// target is invalid for this stream, not a decoder state corruption).
#[derive(Debug, Clone, Copy, derive_more::Display)]
#[display("seek past EOF: new_pos={new_pos} len={len} current_pos={current_pos}")]
#[non_exhaustive]
#[derive(derive_more::Error)]
#[error(ignore)]
pub struct StreamSeekPastEof {
    pub(crate) current_pos: u64,
    pub(crate) len: u64,
    pub(crate) new_pos: u64,
}

impl StreamSeekPastEof {
    /// Build the typed payload for a seek target beyond the stream's
    /// known length.
    #[must_use]
    pub const fn new(current_pos: u64, len: u64, new_pos: u64) -> Self {
        Self {
            current_pos,
            len,
            new_pos,
        }
    }
}

impl<T: StreamType> Stream<T> {
    /// Prime metadata for a seek target by blocking on
    /// [`Source::wait_range`]`(range, None)` until the range resolves
    /// (`Ready`/`Eof`), a segment fails, or the source's cancel fires —
    /// event-driven, **no** wall-clock budget. Runs on the consumer/seek
    /// thread, never the RT worker. The one-time peer wake re-aims the
    /// prefetch window at the new cursor so the awaited range starts
    /// downloading; give-up authority lives lower in the stack (the
    /// downloader's per-fetch inactivity timeout / the cancel hierarchy),
    /// which is why a timer here would only fire on a legitimately in-flight
    /// fetch. The outcome is advisory; `seek` re-checks `source.len()`
    /// afterwards.
    #[kithara::flash(true)]
    fn prime_seek_range(
        &mut self,
        range: Range<u64>,
        cancel: Option<&crate::ReaderControl>,
    ) -> io::Result<()> {
        if matches!(
            self.source.phase_at(range.clone()),
            SourcePhase::Ready | SourcePhase::Eof
        ) {
            return Ok(());
        }
        if let Some(wake) = self.source.peer_wake() {
            wake.notify_now();
        }
        self.source
            .wait_range(range, None, cancel)
            .map_err(IoError::other)?;
        if cancel.is_some_and(crate::ReaderControl::is_cancelled) {
            return Err(IoError::new(
                ErrorKind::Interrupted,
                PendingReason::SessionRetired,
            ));
        }
        Ok(())
    }

    /// Resolve a [`SeekFrom`] against the live cursor — the shared math
    /// behind the off-RT [`Seek::seek`] and the on-core probe seek
    /// (the audio worker's shared-stream wrapper).
    ///
    /// `len` is the known source length, resolved by the caller (`seek` primes
    /// to discover it; a probe seek uses the current value).
    ///
    /// # Errors
    ///
    /// See [`resolve_seek_target`].
    fn resolve_seek_target(&self, pos: SeekFrom, len: Option<u64>) -> io::Result<u64> {
        resolve_seek_target(pos, self.source.position(), len)
    }
}

/// Resolve a [`SeekFrom`] to an absolute, clamped byte target. `position` is
/// the live cursor (`Current` is relative to it); `SeekFrom::End` errors when
/// `len` is `None`. Shared by [`Stream`]'s seek adapters and lock-free
/// wrappers that seek through a [`crate::SourceProbe`].
///
/// # Errors
///
/// `Unsupported` for `SeekFrom::End` without a known length; `InvalidInput`
/// for a negative resulting offset.
pub fn resolve_seek_target(pos: SeekFrom, position: u64, len: Option<u64>) -> io::Result<u64> {
    let new_pos: i128 = match pos {
        SeekFrom::Start(p) => i128::from(p),
        SeekFrom::Current(delta) => i128::from(position).saturating_add(i128::from(delta)),
        SeekFrom::End(delta) => {
            let Some(len) = len else {
                return Err(IoError::new(
                    ErrorKind::Unsupported,
                    "seek from end requires known length",
                ));
            };
            i128::from(len).saturating_add(i128::from(delta))
        }
    };
    if new_pos < 0 {
        return Err(IoError::new(
            ErrorKind::InvalidInput,
            "negative seek position",
        ));
    }
    Ok(u64::try_from(new_pos).unwrap_or(u64::MAX))
}

impl<T: StreamType> Seek for Stream<T> {
    /// Off the real-time path, discovers the length for an `End`-relative seek by priming, since
    /// `probe_seek` cannot and errors instead. The cursor is published before priming so the peer,
    /// which aims from the cursor, targets the new position rather than the stale one.
    #[kithara::measure]
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.seek_with_control(pos, None)
    }
}

impl<T: StreamType> Stream<T> {
    /// Reposition off-core with cancellation reaching metadata and byte readiness waits.
    ///
    /// # Errors
    /// Returns source, invalid-position, or reader cancellation errors.
    pub fn seek_with_control(
        &mut self,
        pos: SeekFrom,
        cancel: Option<&crate::ReaderControl>,
    ) -> io::Result<u64> {
        if cancel.is_some_and(crate::ReaderControl::is_cancelled) {
            return Err(IoError::new(
                ErrorKind::Interrupted,
                PendingReason::SessionRetired,
            ));
        }
        let current = self.source.position();

        if matches!(pos, SeekFrom::End(_)) && self.source.len().is_none() {
            self.prime_seek_range(0..1, cancel)?;
        }
        let new_pos = self.resolve_seek_target(pos, self.source.len())?;

        self.source.set_position(new_pos);

        self.prime_seek_range(new_pos..new_pos.saturating_add(1), cancel)?;

        if let Some(len) = self.source.len()
            && new_pos > len
        {
            debug!(
                current,
                len,
                new_pos,
                ?pos,
                "refusing a seek past the published end of the stream"
            );
            self.source.set_position(current);
            return Err(IoError::new(
                ErrorKind::InvalidInput,
                StreamSeekPastEof {
                    new_pos,
                    len,
                    current_pos: current,
                },
            ));
        }

        Ok(new_pos)
    }
}
