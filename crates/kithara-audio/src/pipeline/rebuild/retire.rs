use std::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
};

use crossbeam_queue::ArrayQueue;
use kithara_decode::Decoder;
use tracing::warn;

pub(crate) struct RetiredDecoders {
    decoders: ArrayQueue<Box<dyn Decoder>>,
    overflowed: AtomicBool,
}

impl RetiredDecoders {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            decoders: ArrayQueue::new(capacity),
            overflowed: AtomicBool::new(false),
        }
    }

    pub(crate) fn retire(&self, decoder: Box<dyn Decoder>) {
        if let Err(decoder) = self.decoders.push(decoder) {
            self.overflowed.store(true, Ordering::Release);
            mem::forget(decoder);
        }
    }

    pub(crate) fn drain(&self) {
        while self.decoders.pop().is_some() {}
        if self.overflowed.swap(false, Ordering::AcqRel) {
            warn!("decoder retire queue overflowed; leaked decoder to keep RT core free");
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.decoders.len()
    }
}
