use std::{fmt, ops::Range, path::Path};

use bon::bon;
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_storage::{ResourceStatus, StorageError, StorageResult, WaitOutcome};

use super::{contract::ProcessCtx, gate::ReadinessGate, writer::ProcessedWriter};
use crate::{consts, resource::ReadSide};

/// Read view over a resource that exposes bytes only after processing completes.
#[derive_where::derive_where(Clone; R: Clone)]
pub struct ProcessedReader<R, S> {
    readiness: Arc<ReadinessGate>,
    gate_poll_interval: Duration,
    processor: Option<ProcessCtx>,
    pools: PoolRegion<S>,
    inner: R,
    chunk_size: usize,
}

impl<R: fmt::Debug, S> fmt::Debug for ProcessedReader<R, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessedReader")
            .field("inner", &self.inner)
            .field("processor", &self.processor)
            .field("ready", &self.readiness.is_ready())
            .finish_non_exhaustive()
    }
}

#[bon]
impl<R, S> ProcessedReader<R, S>
where
    R: ReadSide,
    S: HasPool<u8>,
{
    fn finish_wait(
        &self,
        outcome: WaitOutcome,
        cancel: Option<&CancelToken>,
    ) -> StorageResult<WaitOutcome> {
        if self.processor.is_none() || outcome != WaitOutcome::Ready {
            return Ok(outcome);
        }
        let ready = cancel.map_or_else(
            || self.readiness.wait_until_ready(&|| self.inner_terminal()),
            |cancel| {
                self.readiness
                    .wait_until_ready_with_cancel(cancel, &|| self.inner_terminal())
            },
        );
        if ready {
            Ok(WaitOutcome::Ready)
        } else if cancel.is_some_and(CancelToken::is_cancelled) {
            Err(StorageError::Cancelled)
        } else {
            Ok(WaitOutcome::Interrupted)
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

    pub(super) fn with_readiness(
        inner: R,
        readiness: Arc<ReadinessGate>,
        processor: Option<ProcessCtx>,
        pools: PoolRegion<S>,
        chunk_size: usize,
        gate_poll_interval: Duration,
    ) -> Self {
        Self {
            readiness,
            gate_poll_interval,
            processor,
            pools,
            inner,
            chunk_size,
        }
    }

    /// Wraps an already-committed resource. `chunk_size` and
    /// `gate_poll_interval` carry the same defaults [`ProcessedWriter`] does.
    #[builder]
    pub(super) fn wrap_ready(
        inner: R,
        processor: Option<ProcessCtx>,
        pools: PoolRegion<S>,
        #[builder(default = consts::DEFAULT_CHUNK_SIZE)] chunk_size: usize,
        #[builder(default = consts::DEFAULT_GATE_POLL_INTERVAL)] gate_poll_interval: Duration,
    ) -> Self {
        let ready =
            processor.is_none() || matches!(inner.status(), ResourceStatus::Committed { .. });
        Self {
            pools,
            chunk_size,
            gate_poll_interval,
            processor,
            inner,
            readiness: Arc::new(ReadinessGate::new(ready, gate_poll_interval)),
        }
    }
}

impl<R, S> ReadSide for ProcessedReader<R, S>
where
    R: ReadSide,
    S: HasPool<u8> + Send + Sync + 'static,
{
    type Writer = ProcessedWriter<R::Writer, S>;

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.is_readable() && self.inner.contains_range(range)
    }

    fn reactivate(self) -> StorageResult<ProcessedWriter<R::Writer, S>> {
        let inner = self.inner.reactivate()?;
        Ok(ProcessedWriter::builder()
            .inner(inner)
            .maybe_processor(self.processor)
            .pools(self.pools)
            .chunk_size(self.chunk_size)
            .gate_poll_interval(self.gate_poll_interval)
            .build())
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if !self.is_readable() {
            return Err(StorageError::NotReadable);
        }
        self.inner.read_at(offset, buf)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        let outcome = self.inner.wait_range(range)?;
        self.finish_wait(outcome, None)
    }

    fn wait_range_with_cancel(
        &self,
        range: Range<u64>,
        cancel: &CancelToken,
    ) -> StorageResult<WaitOutcome> {
        let outcome = self.inner.wait_range_with_cancel(range, cancel)?;
        self.finish_wait(outcome, Some(cancel))
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
}
