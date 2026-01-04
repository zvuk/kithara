//! # Kithara I/O
//!
//! Sync I/O adapters for Kithara.
//!
//! ## Goal
//!
//! Provide `std::io::Read + std::io::Seek` required by consumers like `rodio::Decoder`,
//! **without** depending on `kithara-storage` or `kithara-assets`.
//!
//! `kithara-file` / `kithara-hls` are responsible for fetching bytes and ensuring that
//! requested ranges become available; this crate only adapts a generic async "source"
//! into a sync reader.
//!
//! ## Public contract
//!
//! - [`Source`] — async random-access byte source with "wait until readable" semantics.
//! - [`Reader`] — sync `Read + Seek` adapter over a [`Source`].
//!
//! ## EOF semantics (normative)
//!
//! `Read::read()` returns `Ok(0)` **only** when the source reports EOF for the requested
//! position (i.e. `wait_range(..)` returns [`WaitOutcome::Eof`], or `read_at` returns
//! an empty buffer after EOF is known).
//!
//! No "false EOFs": when data is not yet available, the reader blocks.
//!
//! ## Cancellation
//!
//! This crate does not invent cancellation; the concrete [`Source`] implementation may
//! unblock by returning an error.
//!
//! ## Debugging
//!
//! `Reader` emits `tracing` logs at `trace`/`debug` level to help diagnose stalls/deadlocks.
//! Enable with e.g. `RUST_LOG=kithara_io=trace`.

#![forbid(unsafe_code)]

use std::{
    io::{Read, Seek, SeekFrom},
    ops::Range,
    sync::Arc,
};

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;
use tokio::runtime::Handle;
use tracing::{debug, trace, warn};

/// Outcome of waiting for a byte range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The requested range is available for reading.
    Ready,
    /// The source is at EOF and the requested range starts at/after EOF.
    Eof,
}

/// Error type for `kithara-io`.
#[derive(Debug, Error)]
pub enum IoError {
    #[error("source error: {0}")]
    Source(String),

    #[error("seek requires known length, but source length is unknown")]
    UnknownLength,

    #[error("invalid seek position")]
    InvalidSeek,
}

pub type IoResult<T> = Result<T, IoError>;

/// Async random-access source contract.
///
/// This is intentionally minimal and does **not** depend on any storage implementation.
/// `kithara-file` / `kithara-hls` should implement this for their internal byte providers.
///
/// Normative:
/// - `wait_range(range)` must block until the entire `range` is readable, OR return `Eof`
///   if `range.start` is at/after EOF, OR return error if the source fails/cancels.
/// - `read_at(offset, len)` must return up to `len` bytes without implicitly waiting.
///   If the caller needs blocking semantics it must call `wait_range` first.
/// - When `offset` is at/after EOF (and EOF is known), `read_at` must return `Bytes::new()`.
#[async_trait]
pub trait Source: Send + Sync + 'static {
    async fn wait_range(&self, range: Range<u64>) -> IoResult<WaitOutcome>;
    async fn read_at(&self, offset: u64, len: usize) -> IoResult<Bytes>;

    /// Return known total length if available.
    ///
    /// - `Some(len)` enables `SeekFrom::End(..)` and validation for seeking past EOF.
    /// - `None` means length is unknown (still seekable via absolute positions, but
    ///   seeking relative to end is unsupported).
    fn len(&self) -> Option<u64>;
}

/// Sync `Read + Seek` adapter over a [`Source`].
///
/// This is designed specifically to satisfy consumers like `rodio::Decoder`.
///
/// Blocking behavior:
/// - `read()` blocks on the current Tokio runtime handle to wait for availability.
/// - Do **not** call this from within a Tokio async task (it will block the executor thread).
///   Use it from a dedicated blocking thread (e.g. `tokio::task::spawn_blocking` or `std::thread`).
pub struct Reader<S>
where
    S: Source,
{
    source: Arc<S>,
    handle: Handle,
    pos: u64,
}

impl<S> Reader<S>
where
    S: Source,
{
    /// Create a new reader bound to the current Tokio runtime.
    ///
    /// This captures [`tokio::runtime::Handle::current`], so it must be called from within
    /// a Tokio runtime context (e.g. in an async fn before spawning the blocking consumer).
    pub fn new(source: Arc<S>) -> Self {
        trace!("kithara-io Reader::new (capturing tokio runtime handle)");
        let len = source.len();
        debug!(len, "kithara-io Reader created");
        Self {
            source,
            handle: Handle::current(),
            pos: 0,
        }
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn into_source(self) -> Arc<S> {
        self.source
    }
}

impl<S> Read for Reader<S>
where
    S: Source,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            trace!("Reader::read called with empty buf -> Ok(0)");
            return Ok(0);
        }

        let offset = self.pos;
        let len = buf.len();
        trace!(offset, len, "Reader::read enter");

        let wait_range = offset..offset.saturating_add(len as u64);
        trace!(
            start = wait_range.start,
            end = wait_range.end,
            "Reader::read wait_range begin"
        );
        let outcome = self
            .handle
            .block_on(self.source.wait_range(wait_range))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        trace!(?outcome, "Reader::read wait_range done");

        match outcome {
            WaitOutcome::Eof => {
                debug!(offset, "Reader::read reached EOF");
                return Ok(0);
            }
            WaitOutcome::Ready => {}
        }

        trace!(offset, len, "Reader::read read_at begin");
        let bytes = self
            .handle
            .block_on(self.source.read_at(offset, len))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        trace!(got = bytes.len(), "Reader::read read_at done");

        if bytes.is_empty() {
            // Defensive: if the source returns empty after reporting Ready, treat as EOF-ish.
            // This avoids infinite loops in consumers.
            warn!(
                offset,
                len, "Reader::read got empty buffer after Ready; treating as EOF-ish"
            );
            return Ok(0);
        }

        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        self.pos = self.pos.saturating_add(n as u64);
        trace!(n, new_pos = self.pos, "Reader::read exit");
        Ok(n)
    }
}

impl<S> Seek for Reader<S>
where
    S: Source,
{
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        trace!(cur = self.pos, ?pos, "Reader::seek enter");

        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (self.pos as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.source.len() else {
                    debug!("Reader::seek from end requested but source len is unknown");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        IoError::UnknownLength.to_string(),
                    ));
                };
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            debug!(new_pos, "Reader::seek invalid (negative)");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                IoError::InvalidSeek.to_string(),
            ));
        }

        let new_pos_u64 = new_pos as u64;

        // If we know length, disallow seeking past EOF to match typical `Read+Seek` expectations
        // for decoders that probe file structure.
        if let Some(len) = self.source.len() {
            if new_pos_u64 > len {
                debug!(new_pos_u64, len, "Reader::seek invalid (past EOF)");
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    IoError::InvalidSeek.to_string(),
                ));
            }
        }

        self.pos = new_pos_u64;
        trace!(new_pos = self.pos, "Reader::seek exit");
        Ok(self.pos)
    }
}
