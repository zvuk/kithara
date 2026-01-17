//! Sync Read+Seek adapter over async Source.
//!
//! Simple blocking adapter without prefetch.
//! HLS already buffers segments ahead, so data is usually ready.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

use kithara_stream::{MediaInfo, Source, StreamError, WaitOutcome};
use tokio::runtime::Handle;

/// Sync reader over async Source.
///
/// Uses `block_on` to wait for data. Simple and direct.
pub struct SourceReader<S: Source> {
    source: Arc<S>,
    pos: u64,
    rt: Handle,
}

impl<S: Source> SourceReader<S> {
    /// Create a new reader.
    ///
    /// Must be called from within a Tokio runtime context.
    pub fn new(source: Arc<S>) -> Self {
        Self {
            source,
            pos: 0,
            rt: Handle::current(),
        }
    }

    /// Get current media info from source.
    ///
    /// This reflects the media info of data currently being read.
    pub fn media_info(&self) -> Option<MediaInfo> {
        self.source.media_info()
    }

    /// Get current position.
    pub fn position(&self) -> u64 {
        self.pos
    }
}

impl<S: Source> Read for SourceReader<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let range = self.pos..self.pos.saturating_add(buf.len() as u64);

        // Block on async wait and read
        let result = self.rt.block_on(async {
            // Wait for data to be available
            match self.source.wait_range(range).await {
                Ok(WaitOutcome::Ready) => {}
                Ok(WaitOutcome::Eof) => return Ok(0),
                Err(e) => {
                    return Err(std::io::Error::other(e.to_string()));
                }
            }

            // Read data
            match self.source.read_at(self.pos, buf).await {
                Ok(n) => Ok(n),
                Err(e) => Err(std::io::Error::other(e.to_string())),
            }
        });

        match result {
            Ok(n) => {
                self.pos = self.pos.saturating_add(n as u64);
                Ok(n)
            }
            Err(e) => Err(e),
        }
    }
}

impl<S: Source> Seek for SourceReader<S> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (self.pos as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.source.len() else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        StreamError::<S::Error>::UnknownLength.to_string(),
                    ));
                };
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                StreamError::<S::Error>::InvalidSeek.to_string(),
            ));
        }

        let new_pos_u64 = new_pos as u64;

        if let Some(len) = self.source.len()
            && new_pos_u64 > len
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                StreamError::<S::Error>::InvalidSeek.to_string(),
            ));
        }

        self.pos = new_pos_u64;
        Ok(self.pos)
    }
}
