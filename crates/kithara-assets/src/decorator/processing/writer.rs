use std::fmt;

use bon::bon;
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_storage::{StorageError, StorageResult};

use super::{contract::ProcessCtx, gate::ReadinessGate, guard::GateGuard, reader::ProcessedReader};
use crate::{
    consts,
    resource::{RawWriteHandle, ReadSide, WriteSide},
};

fn run_process<W, S>(
    pools: &PoolRegion<S>,
    processor: &ProcessCtx,
    inner: &W,
    final_len: u64,
    chunk_size: usize,
) -> StorageResult<u64>
where
    W: WriteSide,
    S: HasPool<u8>,
{
    let chunk_size_u64 = chunk_size as u64;
    let mut sink = processor.begin();
    let raw = inner.reader();

    let mut input_buf = pools.get_with_len::<u8>(chunk_size).map_err(|error| {
        StorageError::Failed(format!("processing input buffer growth failed: {error}"))
    })?;
    let mut output_buf = pools.get_with_len::<u8>(chunk_size).map_err(|error| {
        StorageError::Failed(format!("processing output buffer growth failed: {error}"))
    })?;

    let mut read_offset = 0u64;
    let mut write_offset = 0u64;

    while read_offset < final_len {
        let remaining_u64 = (final_len - read_offset).min(chunk_size_u64);
        let to_read = usize::try_from(remaining_u64).map_err(|err| {
            StorageError::Failed(format!(
                "process_and_write: chunk size {remaining_u64} does not fit usize: {err}"
            ))
        })?;
        let is_last = read_offset + remaining_u64 >= final_len;

        let read = raw.read_inflight_at(read_offset, &mut input_buf[..to_read])?;
        if read == 0 {
            break;
        }

        let written = sink
            .process(&input_buf[..read], &mut output_buf[..read], is_last)
            .map_err(StorageError::Failed)?;

        inner.write_at(write_offset, &output_buf[..written])?;

        read_offset += read as u64;
        write_offset += u64::try_from(written).map_err(|err| {
            StorageError::Failed(format!(
                "process_and_write: written {written} does not fit u64: {err}"
            ))
        })?;
    }

    Ok(write_offset)
}

/// Write handle that processes a resource atomically when it is committed.
pub struct ProcessedWriter<W, S> {
    gate_poll_interval: Duration,
    guard: GateGuard,
    processor: Option<ProcessCtx>,
    pools: PoolRegion<S>,
    inner: W,
    chunk_size: usize,
}

impl<W: fmt::Debug, S> fmt::Debug for ProcessedWriter<W, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessedWriter")
            .field("inner", &self.inner)
            .field("processor", &self.processor)
            .field("ready", &self.guard.is_ready())
            .finish_non_exhaustive()
    }
}

#[bon]
impl<W, S> ProcessedWriter<W, S>
where
    W: WriteSide,
    S: HasPool<u8>,
{
    /// Creates a pending writer for a resource and its optional processor.
    /// `chunk_size` and `gate_poll_interval` carry the production defaults, so
    /// a caller states them only when it wants a different pass size or
    /// recheck cadence.
    #[builder]
    pub fn new(
        inner: W,
        processor: Option<ProcessCtx>,
        pools: PoolRegion<S>,
        #[builder(default = consts::DEFAULT_CHUNK_SIZE)] chunk_size: usize,
        #[builder(default = consts::DEFAULT_GATE_POLL_INTERVAL)] gate_poll_interval: Duration,
    ) -> Self {
        let ready = processor.is_none();
        Self {
            inner,
            processor,
            pools,
            chunk_size,
            gate_poll_interval,
            guard: GateGuard::new(Arc::new(ReadinessGate::new(ready, gate_poll_interval))),
        }
    }

    fn build_reader(&self, inner: W::Reader) -> ProcessedReader<W::Reader, S> {
        ProcessedReader::with_readiness(
            inner,
            self.guard.shared(),
            self.processor.clone(),
            self.pools.clone(),
            self.chunk_size,
            self.gate_poll_interval,
        )
    }
}

impl<W, S> WriteSide for ProcessedWriter<W, S>
where
    W: WriteSide,
    S: HasPool<u8> + Send + Sync + 'static,
{
    type Reader = ProcessedReader<W::Reader, S>;

    /// Disarming without failing avoids reaching the successor's readers: the readiness gate is
    /// shared with this generation, and the cancel that triggered this is tearing only this
    /// generation down.
    fn abandon(mut self) {
        self.inner.abandon();
        self.guard.disarm();
    }

    fn commit(mut self, final_len: Option<u64>) -> StorageResult<ProcessedReader<W::Reader, S>> {
        let needs_processing = self.processor.is_some() && !self.guard.is_ready();
        let actual_len = match (needs_processing, final_len, self.processor.as_ref()) {
            (true, Some(len), Some(processor)) if len > 0 => Some(run_process(
                &self.pools,
                processor,
                &self.inner,
                len,
                self.chunk_size,
            )?),
            _ => final_len,
        };

        let reader_inner = self.inner.commit(actual_len)?;
        if needs_processing {
            self.guard.mark_ready();
        }
        self.guard.disarm();
        Ok(ProcessedReader::with_readiness(
            reader_inner,
            self.guard.shared(),
            self.processor.clone(),
            self.pools.clone(),
            self.chunk_size,
            self.gate_poll_interval,
        ))
    }

    fn fail(mut self, reason: String) {
        self.inner.fail(reason);
        self.guard.fail();
        self.guard.disarm();
    }

    fn reader(&self) -> ProcessedReader<W::Reader, S> {
        self.build_reader(self.inner.reader())
    }

    delegate::delegate! {
        to self.inner {
            fn raw_write_handle(&self) -> RawWriteHandle;
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }
}
