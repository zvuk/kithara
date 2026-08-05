use std::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
};

use crossbeam_queue::ArrayQueue;
use kithara_decode::{ChunkSink, PcmChunk};
use tracing::warn;

use crate::pipeline::decode::DecoderGeneration;

/// Decode state the produce core displaced and must not free: whole
/// generations replaced by a rebuild, and the chunks a seek flushed out of the
/// staging and gapless buffers. Both are pushed lock-free on the core and
/// dropped by [`drain`](Self::drain) in the worker shell.
pub(crate) struct Retired {
    chunks: ArrayQueue<PcmChunk>,
    generations: ArrayQueue<DecoderGeneration>,
    overflowed: AtomicBool,
}

impl Retired {
    pub(crate) fn new(generations: usize, chunks: usize) -> Self {
        Self {
            chunks: ArrayQueue::new(chunks),
            generations: ArrayQueue::new(generations),
            overflowed: AtomicBool::new(false),
        }
    }

    pub(crate) fn drain(&self) {
        while self.chunks.pop().is_some() {}
        while self.generations.pop().is_some() {}
        if self.overflowed.swap(false, Ordering::AcqRel) {
            warn!("decode retire queue overflowed; leaked retired state to keep RT core free");
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.generations.len()
    }

    #[cfg(test)]
    pub(crate) fn chunk_len(&self) -> usize {
        self.chunks.len()
    }

    pub(crate) fn retire_generation(&self, generation: DecoderGeneration) {
        if let Err(generation) = self.generations.push(generation) {
            self.overflowed.store(true, Ordering::Release);
            mem::forget(generation);
        }
    }
}

impl ChunkSink for Retired {
    fn retire(&self, chunk: PcmChunk) {
        if let Err(chunk) = self.chunks.push(chunk) {
            self.overflowed.store(true, Ordering::Release);
            mem::forget(chunk);
        }
    }
}
