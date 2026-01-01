#![forbid(unsafe_code)]

use std::io::{Read, Seek};

use bytes::Bytes;
use kanal::{Receiver, Sender};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IoError {
    #[error("not implemented")]
    Unimplemented,
}

pub type IoResult<T> = Result<T, IoError>;

#[derive(Clone, Debug)]
pub struct BridgeOptions {
    pub max_buffer_bytes: usize,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            max_buffer_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BridgeWriter {
    _tx: Sender<BridgeMsg>,
}

impl BridgeWriter {
    pub fn push(&self, _bytes: Bytes) -> IoResult<()> {
        unimplemented!("kithara-io: BridgeWriter::push is not implemented yet")
    }

    pub fn finish(&self) -> IoResult<()> {
        unimplemented!("kithara-io: BridgeWriter::finish is not implemented yet")
    }
}

#[derive(Debug)]
pub struct BridgeReader {
    _rx: Receiver<BridgeMsg>,
}

impl Read for BridgeReader {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        unimplemented!("kithara-io: BridgeReader::read is not implemented yet")
    }
}

impl Seek for BridgeReader {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        unimplemented!("kithara-io: BridgeReader::seek is not implemented yet")
    }
}

#[derive(Debug)]
enum BridgeMsg {
    Data(Bytes),
    EndOfStream,
}

pub fn new_bridge(_opts: BridgeOptions) -> (BridgeWriter, BridgeReader) {
    unimplemented!("kithara-io: new_bridge is not implemented yet")
}
