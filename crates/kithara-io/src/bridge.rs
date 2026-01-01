#[derive(Debug)]
pub enum BridgeMsg {
    DataWithSize(Bytes, usize),
    EndOfStream,
}

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

pub fn new_bridge(opts: BridgeOptions) -> (BridgeWriter, BridgeReader) {
    let (tx, rx) = kanal::bounded(1000); // Large enough for messages, we handle byte limiting ourselves
    let buffer_tracker = BufferTracker::new(opts.max_buffer_bytes);
    let writer = BridgeWriter {
        tx,
        max_buffer_bytes: opts.max_buffer_bytes,
        current_buffer_bytes: buffer_tracker.tracker(),
    };
    let reader = BridgeReader::new(rx, buffer_tracker.tracker());
    (writer, reader)
}

use bytes::Bytes;

use crate::reader::BridgeReader;
use crate::sync::BufferTracker;
use crate::writer::BridgeWriter;
