#![forbid(unsafe_code)]

//! Sync reader with random access.
//!
//! `Reader<B>` implements `Read + Seek` using a backend.
//! No `block_on` - uses kanal's native sync operations.

use std::io::{Read, Seek, SeekFrom};

use bytes::Bytes;

use crate::backend::{BackendAccess, Command, Response};

/// Sync reader with random access.
///
/// Uses `BackendAccess` trait for actual I/O.
/// Implements `Read + Seek` for use with symphonia.
pub struct Reader<B: BackendAccess> {
    backend: B,
    pos: u64,
    buffer: Bytes,
    buffer_offset: usize,
    eof: bool,
}

impl<B: BackendAccess> Reader<B> {
    /// Create new reader.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            pos: 0,
            buffer: Bytes::new(),
            buffer_offset: 0,
            eof: false,
        }
    }

    /// Get current position.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Get total length if known.
    pub fn len(&self) -> Option<u64> {
        self.backend.len()
    }

    /// Check if length is zero or unknown.
    pub fn is_empty(&self) -> bool {
        self.backend.len().map(|l| l == 0).unwrap_or(true)
    }

    /// Get media info if known.
    pub fn media_info(&self) -> Option<crate::MediaInfo> {
        self.backend.media_info()
    }

    /// Get current segment byte range.
    pub fn current_segment_range(&self) -> std::ops::Range<u64> {
        self.backend.current_segment_range()
    }

    /// Request data from backend and wait for response.
    fn request_and_receive(&mut self, len: usize) -> std::io::Result<Option<Bytes>> {
        self.backend
            .command_tx()
            .send(Command::Read {
                offset: self.pos,
                len,
            })
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "backend stopped")
            })?;

        match self.backend.response_rx().recv() {
            Ok(Response::Data(bytes)) => Ok(Some(bytes)),
            Ok(Response::Eof) => Ok(None),
            Ok(Response::Error(msg)) => {
                Err(std::io::Error::other(msg))
            }
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "backend channel closed",
            )),
        }
    }
}

impl<B: BackendAccess> Read for Reader<B> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() || self.eof {
            tracing::debug!(pos = self.pos, eof = self.eof, buf_empty = buf.is_empty(), "Reader.read early return");
            return Ok(0);
        }

        // First, drain any buffered data
        if self.buffer_offset < self.buffer.len() {
            let remaining = &self.buffer[self.buffer_offset..];
            let to_copy = remaining.len().min(buf.len());
            buf[..to_copy].copy_from_slice(&remaining[..to_copy]);
            self.buffer_offset += to_copy;
            self.pos += to_copy as u64;
            return Ok(to_copy);
        }

        tracing::debug!(pos = self.pos, buf_len = buf.len(), "Reader.read requesting data");

        // Buffer exhausted, request new data
        match self.request_and_receive(buf.len())? {
            Some(bytes) => {
                if bytes.is_empty() {
                    tracing::debug!(pos = self.pos, "Reader.read: empty bytes, setting eof");
                    self.eof = true;
                    return Ok(0);
                }

                let to_copy = bytes.len().min(buf.len());
                buf[..to_copy].copy_from_slice(&bytes[..to_copy]);
                self.pos += to_copy as u64;

                // Buffer remaining if any
                if to_copy < bytes.len() {
                    self.buffer = bytes;
                    self.buffer_offset = to_copy;
                } else {
                    self.buffer = Bytes::new();
                    self.buffer_offset = 0;
                }

                Ok(to_copy)
            }
            None => {
                tracing::debug!(pos = self.pos, "Reader.read: None response, setting eof");
                self.eof = true;
                Ok(0)
            }
        }
    }
}

impl<B: BackendAccess> Seek for Reader<B> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (self.pos as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.backend.len() else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "seek from end requires known length",
                    ));
                };
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }

        let new_pos = new_pos as u64;

        if let Some(len) = self.backend.len()
            && new_pos > len
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek past EOF",
            ));
        }

        // Send seek command to backend
        self.backend
            .command_tx()
            .send(Command::Seek { offset: new_pos })
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "backend stopped")
            })?;

        // Clear buffer and reset state
        self.buffer = Bytes::new();
        self.buffer_offset = 0;
        self.pos = new_pos;
        self.eof = false;

        Ok(new_pos)
    }
}

impl<B: BackendAccess> Drop for Reader<B> {
    fn drop(&mut self) {
        let _ = self.backend.command_tx().send(Command::Stop);
    }
}
