use std::io::{Read, Seek};

#[derive(Debug)]
pub struct BridgeReader {
    rx: Receiver<BridgeMsg>,
    current_data: Option<Bytes>,
    read_pos: usize,
    eos_received: bool,
    current_data_size: usize,
    buffer_tracker: std::sync::Arc<BufferTracker>,
}

impl BridgeReader {
    pub fn new(rx: Receiver<BridgeMsg>, buffer_tracker: std::sync::Arc<BufferTracker>) -> Self {
        Self {
            rx,
            current_data: None,
            read_pos: 0,
            eos_received: false,
            current_data_size: 0,
            buffer_tracker,
        }
    }
}

impl Read for BridgeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // If we have current data, try to read from it
        if let Some(ref data) = self.current_data {
            let remaining = data.len() - self.read_pos;
            if remaining > 0 {
                let to_copy = std::cmp::min(remaining, buf.len());
                buf[..to_copy].copy_from_slice(&data[self.read_pos..self.read_pos + to_copy]);
                self.read_pos += to_copy;

                if self.read_pos == data.len() {
                    // Release bytes from buffer tracking
                    self.buffer_tracker.release(self.current_data_size);
                    self.current_data = None;
                    self.read_pos = 0;
                    self.current_data_size = 0;
                }

                return Ok(to_copy);
            }
        }

        // If we've received EOS and no more data, return 0 (EOF)
        if self.eos_received {
            return Ok(0);
        }

        // Block waiting for next message
        match self.rx.recv() {
            Ok(BridgeMsg::DataWithSize(bytes, size)) => {
                self.current_data = Some(bytes);
                self.current_data_size = size;
                self.read_pos = 0;
                // Recurse to read from the new data
                self.read(buf)
            }
            Ok(BridgeMsg::EndOfStream) => {
                self.eos_received = true;
                // No more data, return EOF
                Ok(0)
            }
            Err(_) => {
                // Channel closed - treat as EOF
                self.eos_received = true;
                Ok(0)
            }
        }
    }
}

impl Seek for BridgeReader {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "seek is not supported in kithara-io bridge",
        ))
    }
}

use bytes::Bytes;
use kanal::Receiver;

use crate::{bridge::BridgeMsg, sync::BufferTracker};
