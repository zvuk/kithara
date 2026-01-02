#[derive(Clone, Debug)]
pub struct BridgeWriter {
    pub(crate) tx: Sender<BridgeMsg>,
    pub(crate) buffer_tracker: std::sync::Arc<BufferTracker>,
}

impl BridgeWriter {
    pub fn push(&self, bytes: Bytes) -> IoResult<()> {
        let bytes_len = bytes.len();

        // Check and reserve space using BufferTracker
        self.buffer_tracker.try_reserve(bytes_len)?;
        self.buffer_tracker.reserve(bytes_len);

        // Send message with size tracking
        let msg = BridgeMsg::DataWithSize(bytes, bytes_len);
        self.tx.send(msg).map_err(|_| {
            // If send fails, release the reserved space
            self.buffer_tracker.release(bytes_len);
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
use crate::sync::BufferTracker;
