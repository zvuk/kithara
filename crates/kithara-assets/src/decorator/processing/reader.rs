use std::{fmt, ops::Range, path::Path};

use kithara_bufpool::BytePool;
use kithara_platform::sync::Arc;
use kithara_storage::{ResourceStatus, StorageError, StorageResult, WaitOutcome};

use super::{contract::ProcessCtx, gate::ReadinessGate, writer::ProcessedWriter};
use crate::resource::ReadSide;

/// Read view over a resource that exposes bytes only after processing completes.
pub struct ProcessedReader<R> {
    readiness: Arc<ReadinessGate>,
    pool: BytePool,
    processor: Option<ProcessCtx>,
    inner: R,
}

impl<R> Clone for ProcessedReader<R>
where
    R: Clone,
{
    fn clone(&self) -> Self {
        Self {
            readiness: Arc::clone(&self.readiness),
            pool: self.pool.clone(),
            processor: self.processor.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl<R: fmt::Debug> fmt::Debug for ProcessedReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessedReader")
            .field("inner", &self.inner)
            .field("processor", &self.processor)
            .field("ready", &self.readiness.is_ready())
            .finish_non_exhaustive()
    }
}

impl<R> ProcessedReader<R>
where
    R: ReadSide,
{
    pub(super) fn with_readiness(
        inner: R,
        readiness: Arc<ReadinessGate>,
        processor: Option<ProcessCtx>,
        pool: BytePool,
    ) -> Self {
        Self {
            readiness,
            pool,
            processor,
            inner,
        }
    }

    pub(super) fn wrap_ready(inner: R, processor: Option<ProcessCtx>, pool: BytePool) -> Self {
        let ready =
            processor.is_none() || matches!(inner.status(), ResourceStatus::Committed { .. });
        Self {
            pool,
            processor,
            inner,
            readiness: Arc::new(ReadinessGate::new(ready)),
        }
    }

    fn inner_terminal(&self) -> bool {
        self.readiness.is_failed()
            || matches!(
                self.inner.status(),
                ResourceStatus::Failed(_) | ResourceStatus::Cancelled
            )
    }

    fn is_readable(&self) -> bool {
        self.processor.is_none() || self.readiness.is_ready()
    }
}

impl<R> ReadSide for ProcessedReader<R>
where
    R: ReadSide,
{
    type Writer = ProcessedWriter<R::Writer>;

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.is_readable() && self.inner.contains_range(range)
    }

    delegate::delegate! {
        to self.inner {
            fn len(&self) -> Option<u64>;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
            fn path(&self) -> Option<&Path>;
            fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn status(&self) -> ResourceStatus;
        }
    }

    fn reactivate(self) -> StorageResult<ProcessedWriter<R::Writer>> {
        let inner = self.inner.reactivate()?;
        Ok(ProcessedWriter::new(inner, self.processor, self.pool))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if !self.is_readable() {
            return Err(StorageError::NotReadable);
        }
        self.inner.read_at(offset, buf)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        let outcome = self.inner.wait_range(range)?;
        if self.processor.is_none() || outcome != WaitOutcome::Ready {
            return Ok(outcome);
        }
        if self.readiness.wait_until_ready(&|| self.inner_terminal()) {
            Ok(WaitOutcome::Ready)
        } else {
            Ok(WaitOutcome::Interrupted)
        }
    }
}
