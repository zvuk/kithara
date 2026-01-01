#[derive(Clone, Debug)]
pub struct BridgeWriter {
    pub(crate) tx: Sender<BridgeMsg>,
    pub(crate) max_buffer_bytes: usize,
    pub(crate) current_buffer_bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl BridgeWriter {
    pub fn push(&self, bytes: Bytes) -> IoResult<()> {
        let bytes_len = bytes.len();

        // Check if adding this would exceed buffer limit
        let current = self
            .current_buffer_bytes
            .load(std::sync::atomic::Ordering::Acquire);
        if current + bytes_len > self.max_buffer_bytes {
            return Err(IoError::BufferFull);
        }

        // Reserve space
        self.current_buffer_bytes
            .fetch_add(bytes_len, std::sync::atomic::Ordering::AcqRel);

        // Send message with size tracking
        let msg = BridgeMsg::DataWithSize(bytes, bytes_len);
        self.tx.send(msg).map_err(|_| {
            // If send fails, release the reserved space
            self.current_buffer_bytes
                .fetch_sub(bytes_len, std::sync::atomic::Ordering::AcqRel);
            IoError::ChannelClosed
        })
    }

    pub fn finish(&self) -> IoResult<()> {
        self.tx
            .send(BridgeMsg::EndOfStream)
            .map_err(|_| IoError::ChannelClosed)
    }
}

use bytes::Bytes;
use kanal::Sender;

use crate::bridge::BridgeMsg;
use crate::errors::{IoError, IoResult};
