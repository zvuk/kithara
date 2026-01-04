use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
pub struct BufferTracker {
    current_buffer_bytes: Arc<AtomicUsize>,
    max_buffer_bytes: usize,
}

impl BufferTracker {
    pub fn new(max_buffer_bytes: usize) -> Self {
        Self {
            current_buffer_bytes: Arc::new(AtomicUsize::new(0)),
            max_buffer_bytes,
        }
    }

    pub fn tracker(&self) -> Arc<AtomicUsize> {
        self.current_buffer_bytes.clone()
    }

    pub fn try_reserve(&self, bytes_len: usize) -> IoResult<()> {
        let current = self.current_buffer_bytes.load(Ordering::Acquire);
        if current + bytes_len > self.max_buffer_bytes {
            return Err(IoError::BufferFull);
        }
        Ok(())
    }

    pub fn reserve(&self, bytes_len: usize) {
        self.current_buffer_bytes
            .fetch_add(bytes_len, Ordering::AcqRel);
    }

    pub fn release(&self, bytes_len: usize) {
        self.current_buffer_bytes
            .fetch_sub(bytes_len, Ordering::AcqRel);
    }
}

use crate::errors::{IoError, IoResult};
