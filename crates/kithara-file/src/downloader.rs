//! Background file downloader and I/O executor.
//!
//! Contains `FileDownloader` implementing `Downloader` trait,
//! `FileIo` implementing `DownloaderIo`, and supporting types.

use std::sync::Arc;

use futures::StreamExt;
use kithara_assets::{AssetResource, CoverageIndex, DiskCoverage};
use kithara_events::{EventBus, FileEvent};
use kithara_net::{HttpClient, RangeSpec};
use kithara_storage::{Coverage, MemCoverage, MmapResource, ResourceExt};
use kithara_stream::{Downloader, DownloaderIo, PlanOutcome, StepResult, Writer, WriterItem};
use tokio_util::sync::CancellationToken;

use crate::session::{FileStreamState, Progress, SharedFileState};

// FileIo — pure I/O executor (Clone, no &mut self)

/// Pure I/O executor for file range fetching.
#[derive(Clone)]
pub struct FileIo {
    net_client: HttpClient,
    url: url::Url,
    res: AssetResource,
    cancel: CancellationToken,
}

/// Plan for downloading a file range.
pub struct FilePlan {
    pub(crate) range: std::ops::Range<u64>,
}

/// Result of a file download operation.
pub enum FileFetch {
    /// A chunk was written during sequential streaming.
    Chunk { offset: u64, len: u64 },
    /// A range request completed.
    RangeDone { range: std::ops::Range<u64> },
    /// Sequential stream ended.
    StreamEnded { total_bytes: u64 },
}

/// File download error.
#[derive(Debug)]
pub struct FileDownloadError {
    msg: String,
}

impl std::fmt::Display for FileDownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "file download error: {}", self.msg)
    }
}

impl std::error::Error for FileDownloadError {}

impl DownloaderIo for FileIo {
    type Plan = FilePlan;
    type Fetch = FileFetch;
    type Error = FileDownloadError;

    async fn fetch(&self, plan: Self::Plan) -> Result<Self::Fetch, Self::Error> {
        let range = plan.range;
        let spec = RangeSpec::new(range.start, Some(range.end.saturating_sub(1)));

        match self
            .net_client
            .get_range(self.url.clone(), spec, None)
            .await
        {
            Ok(stream) => {
                let mut writer =
                    Writer::with_offset(stream, self.res.clone(), self.cancel.clone(), range.start);

                while let Some(result) = writer.next().await {
                    match result {
                        Ok(WriterItem::ChunkWritten { .. }) => {}
                        Ok(WriterItem::StreamEnded { .. }) => break,
                        Err(e) => {
                            tracing::warn!(?e, "range download failed");
                            break;
                        }
                    }
                }
                tracing::debug!(start = range.start, end = range.end, "range fetched");
                Ok(FileFetch::RangeDone {
                    range: range.clone(),
                })
            }
            Err(e) => {
                tracing::warn!(?e, "failed to start range request");
                Err(FileDownloadError { msg: e.to_string() })
            }
        }
    }
}

// FileCoverage — dual-backend coverage tracker

/// Coverage tracker that works with both in-memory and disk-backed storage.
///
/// Remote files use `Disk` for crash-safe persistence; local files and tests
/// use `Memory`.
pub(crate) enum FileCoverage {
    Memory(MemCoverage),
    Disk(DiskCoverage<MmapResource>),
}

impl Coverage for FileCoverage {
    fn mark(&mut self, range: std::ops::Range<u64>) {
        match self {
            Self::Memory(c) => c.mark(range),
            Self::Disk(c) => c.mark(range),
        }
    }

    fn is_complete(&self) -> bool {
        match self {
            Self::Memory(c) => c.is_complete(),
            Self::Disk(c) => c.is_complete(),
        }
    }

    fn next_gap(&self, max_size: u64) -> Option<std::ops::Range<u64>> {
        match self {
            Self::Memory(c) => c.next_gap(max_size),
            Self::Disk(c) => c.next_gap(max_size),
        }
    }

    fn gaps(&self) -> Vec<std::ops::Range<u64>> {
        match self {
            Self::Memory(c) => c.gaps(),
            Self::Disk(c) => c.gaps(),
        }
    }

    fn total_size(&self) -> Option<u64> {
        match self {
            Self::Memory(c) => c.total_size(),
            Self::Disk(c) => c.total_size(),
        }
    }

    fn set_total_size(&mut self, size: u64) {
        match self {
            Self::Memory(c) => c.set_total_size(size),
            Self::Disk(c) => c.set_total_size(size),
        }
    }
}

impl FileCoverage {
    /// Flush dirty coverage to disk (no-op for in-memory).
    fn flush(&mut self) {
        if let Self::Disk(c) = self {
            c.flush();
        }
    }
}

// FileDownloader — mutable state (plan + commit)

/// Current phase of file download.
enum FilePhase {
    /// Sequential download from start.
    Sequential,
    /// Filling gaps after sequential ended (partial/error).
    GapFilling,
    /// Download complete.
    Complete,
}

/// Background file downloader implementing `Downloader`.
///
/// Supports three phases:
/// - Sequential: initial download from start (streaming step mode)
/// - `GapFilling`: fill holes via HTTP Range requests (batch mode)
/// - Complete: all data downloaded
pub struct FileDownloader {
    io: FileIo,
    writer: Writer,
    res: AssetResource,
    progress: Arc<Progress>,
    bus: EventBus,
    total: Option<u64>,
    /// Backpressure threshold. None = no backpressure.
    look_ahead_bytes: Option<u64>,
    shared: Arc<SharedFileState>,
    /// Coverage tracker for downloaded ranges.
    coverage: FileCoverage,
    /// Current download phase.
    phase: FilePhase,
}

impl FileDownloader {
    pub(crate) async fn new(
        net_client: &HttpClient,
        state: Arc<FileStreamState>,
        progress: Arc<Progress>,
        bus: EventBus,
        look_ahead_bytes: Option<u64>,
        shared: Arc<SharedFileState>,
        coverage_index: Option<Arc<CoverageIndex<MmapResource>>>,
    ) -> Self {
        let url = state.url().clone();
        let total = state.len();
        let res = state.res().clone();
        let cancel = state.cancel().clone();

        let writer = match net_client.stream(url.clone(), None).await {
            Ok(stream) => Writer::new(stream, res.clone(), cancel.clone()),
            Err(e) => {
                tracing::warn!("failed to open stream: {}", e);
                res.fail(e.to_string());
                bus.publish(FileEvent::DownloadError {
                    error: e.to_string(),
                });
                // Return a writer from an empty stream so step() returns error immediately
                let empty = futures::stream::empty();
                let boxed: kithara_net::ByteStream = Box::pin(empty);
                Writer::new(boxed, state.res().clone(), CancellationToken::new())
            }
        };

        let mut coverage: FileCoverage = coverage_index.map_or_else(
            || {
                let mc = total.map_or_else(MemCoverage::new, MemCoverage::with_total_size);
                FileCoverage::Memory(mc)
            },
            |idx| {
                let key = url.to_string();
                let mut dc = DiskCoverage::open(idx, key);
                if let Some(size) = total {
                    dc.set_total_size(size);
                }
                FileCoverage::Disk(dc)
            },
        );

        // If resource already has some data (partial cache) and coverage
        // doesn't know about it yet, mark it. This handles legacy files
        // (downloaded before coverage was introduced).
        if let Some(len) = res.len()
            && len > 0
            && coverage.next_gap(len).is_some_and(|g| g.start == 0)
        {
            coverage.mark(0..len);
        }

        let io = FileIo {
            net_client: net_client.clone(),
            url,
            res: res.clone(),
            cancel,
        };

        Self {
            io,
            writer,
            res,
            progress,
            bus,
            total,
            look_ahead_bytes,
            shared,
            coverage,
            phase: FilePhase::Sequential,
        }
    }
}

impl Downloader for FileDownloader {
    type Plan = FilePlan;
    type Fetch = FileFetch;
    type Error = FileDownloadError;
    type Io = FileIo;

    fn io(&self) -> &Self::Io {
        &self.io
    }

    async fn poll_demand(&mut self) -> Option<FilePlan> {
        let range = self.shared.pop_range_request()?;

        // Cancel sequential download when demand arrives — bandwidth is better
        // spent on the data the reader actually needs (gap-filling).
        if matches!(self.phase, FilePhase::Sequential) {
            tracing::debug!("demand during sequential — switching to gap-filling");
            self.phase = FilePhase::GapFilling;
            // Replace writer with an empty stream to drop the HTTP connection.
            self.writer = Writer::new(
                futures::stream::empty(),
                self.res.clone(),
                CancellationToken::new(),
            );
        }

        tracing::debug!(
            start = range.start,
            end = range.end,
            "processing on-demand range request"
        );
        Some(FilePlan { range })
    }

    async fn plan(&mut self) -> PlanOutcome<FilePlan> {
        match self.phase {
            FilePhase::Sequential => PlanOutcome::Step,
            FilePhase::GapFilling => {
                // Collect up to 4 gaps, each up to 2MB.
                let mut plans = Vec::new();
                let gap_chunk_size: u64 = 2 * 1024 * 1024;
                let gap_batch_size: usize = 4;

                for _ in 0..gap_batch_size {
                    if let Some(gap) = self.coverage.next_gap(gap_chunk_size) {
                        // Skip gaps that overlap with already-planned ranges.
                        plans.push(FilePlan { range: gap });
                    } else {
                        break;
                    }
                }

                if plans.is_empty() {
                    // No more gaps — check if complete.
                    if self.coverage.is_complete() {
                        if let Err(e) = self.res.commit(self.total) {
                            tracing::error!(?e, "failed to commit resource after gap-filling");
                            self.res.fail(format!("commit failed: {e}"));
                            self.bus.publish(FileEvent::DownloadError {
                                error: format!("commit failed: {e}"),
                            });
                        } else if let Some(total) = self.total {
                            self.bus
                                .publish(FileEvent::DownloadComplete { total_bytes: total });
                        }
                        self.phase = FilePhase::Complete;
                        PlanOutcome::Complete
                    } else {
                        // No gaps found but not complete — wait for on-demand requests.
                        // This can happen when total_size is unknown.
                        tokio::select! {
                            () = self.shared.reader_needs_data.notified() => {
                                PlanOutcome::Step // Re-check on next iteration
                            }
                        }
                    }
                } else {
                    PlanOutcome::Batch(plans)
                }
            }
            FilePhase::Complete => PlanOutcome::Complete,
        }
    }

    async fn step(&mut self) -> Result<StepResult<FileFetch>, FileDownloadError> {
        // If sequential ended, wait for on-demand or transition.
        if matches!(self.phase, FilePhase::GapFilling | FilePhase::Complete) {
            return Ok(StepResult::PhaseChange);
        }

        // Sequential download continues.
        let Some(result) = self.writer.next().await else {
            // Writer exhausted (empty stream case).
            self.phase = FilePhase::Complete;
            return Ok(StepResult::PhaseChange);
        };

        match result {
            Ok(WriterItem::ChunkWritten {
                offset,
                len: chunk_len,
            }) => Ok(StepResult::Item(FileFetch::Chunk {
                offset,
                len: chunk_len as u64,
            })),
            Ok(WriterItem::StreamEnded { total_bytes }) => {
                Ok(StepResult::Item(FileFetch::StreamEnded { total_bytes }))
            }
            Err(e) => {
                tracing::warn!("download failed: {}", e);
                self.bus.publish(FileEvent::DownloadError {
                    error: e.to_string(),
                });
                // Transition to gap-filling on error.
                if self.total.is_some() {
                    self.phase = FilePhase::GapFilling;
                    Ok(StepResult::PhaseChange)
                } else {
                    Err(FileDownloadError { msg: e.to_string() })
                }
            }
        }
    }

    fn commit(&mut self, fetch: FileFetch) {
        match fetch {
            FileFetch::Chunk { offset, len } => {
                let download_offset = offset + len;
                self.coverage.mark(offset..download_offset);
                self.progress.set_download_pos(download_offset);
                self.bus.publish(FileEvent::DownloadProgress {
                    offset: download_offset,
                    total: self.total,
                });
            }
            FileFetch::RangeDone { range } => {
                self.coverage.mark(range);
                // Flush after each gap-fill batch for crash-safety.
                self.coverage.flush();
                // Check if gap-filling completed everything.
                if self.coverage.is_complete() {
                    if let Err(e) = self.res.commit(self.total) {
                        tracing::error!(?e, "failed to commit resource after range done");
                        self.res.fail(format!("commit failed: {e}"));
                        self.bus.publish(FileEvent::DownloadError {
                            error: format!("commit failed: {e}"),
                        });
                    } else if let Some(total) = self.total {
                        self.bus
                            .publish(FileEvent::DownloadComplete { total_bytes: total });
                    }
                    self.phase = FilePhase::Complete;
                }
            }
            FileFetch::StreamEnded { total_bytes } => {
                // Update coverage total if not set.
                if self.coverage.total_size().is_none() {
                    self.coverage.set_total_size(total_bytes);
                }

                let is_complete = self.total.is_none_or(|expected| total_bytes >= expected);

                if is_complete {
                    // Complete download — commit resource.
                    if let Err(e) = self.res.commit(Some(total_bytes)) {
                        tracing::error!(?e, "failed to commit resource");
                        self.res.fail(format!("commit failed: {e}"));
                        self.bus.publish(FileEvent::DownloadError {
                            error: format!("commit failed: {e}"),
                        });
                    } else {
                        self.bus
                            .publish(FileEvent::DownloadComplete { total_bytes });
                    }
                    self.phase = FilePhase::Complete;
                } else {
                    // Partial download — switch to gap-filling.
                    // Flush coverage before phase change for crash-safety.
                    self.coverage.flush();
                    tracing::warn!(
                        total_bytes,
                        expected = ?self.total,
                        "stream ended early — switching to gap-filling"
                    );
                    self.bus.publish(FileEvent::DownloadProgress {
                        offset: total_bytes,
                        total: self.total,
                    });
                    self.phase = FilePhase::GapFilling;
                }
            }
        }
    }

    fn should_throttle(&self) -> bool {
        // Only throttle during sequential phase.
        if !matches!(self.phase, FilePhase::Sequential) {
            return false;
        }

        let Some(limit) = self.look_ahead_bytes else {
            return false;
        };

        let download_pos = self.progress.download_pos();
        let reader_pos = self.progress.read_pos();

        download_pos.saturating_sub(reader_pos) > limit
    }

    fn wait_ready(&self) -> impl std::future::Future<Output = ()> + Send {
        // Extract Arc references to avoid capturing &self (which is not Send
        // because Writer contains a non-Sync dyn Stream).
        let progress = Arc::clone(&self.progress);
        let shared = Arc::clone(&self.shared);
        async move {
            tokio::select! {
                () = progress.notified_reader_advance() => {}
                () = shared.reader_needs_data.notified() => {}
            }
        }
    }

    fn demand_signal(&self) -> impl std::future::Future<Output = ()> + Send + use<> {
        let shared = Arc::clone(&self.shared);
        async move {
            shared.reader_needs_data.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_assets::AssetStoreBuilder;
    use kithara_events::EventBus;
    use kithara_net::HttpClient;
    use kithara_storage::MemCoverage;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::session::{Progress, SharedFileState};

    /// After a demand (on-demand range request) is polled during Sequential phase,
    /// the downloader should cancel sequential and switch to gap-filling.
    ///
    /// Currently FAILS: poll_demand does not change phase, so plan() still returns
    /// Step and the sequential download continues wasting bandwidth.
    #[tokio::test]
    async fn sequential_stops_after_demand() {
        let cancel = CancellationToken::new();
        let dir = TempDir::new().unwrap();

        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("test"))
            .cancel(cancel.clone())
            .build();

        let url: url::Url = "http://example.com/test.mp3".parse().unwrap();
        let key = kithara_assets::ResourceKey::from_url(&url);
        let res = store.open_resource(&key).unwrap();

        let total: u64 = 10_000;

        // Pending stream simulates a stalled sequential download.
        let stream = futures::stream::pending::<Result<bytes::Bytes, kithara_net::NetError>>();
        let writer = Writer::new(stream, res.clone(), cancel.clone());

        let io = FileIo {
            net_client: HttpClient::new(Default::default()),
            url: url.clone(),
            res: res.clone(),
            cancel: cancel.clone(),
        };

        let shared = Arc::new(SharedFileState::new());
        let mut coverage = MemCoverage::with_total_size(total);
        coverage.mark(0..1000); // Sequential downloaded first 1KB.

        let mut dl = FileDownloader {
            io,
            writer,
            res,
            progress: Arc::new(Progress::new()),
            bus: EventBus::new(16),
            total: Some(total),
            look_ahead_bytes: None,
            shared: shared.clone(),
            coverage: FileCoverage::Memory(coverage),
            phase: FilePhase::Sequential,
        };

        // Seek far ahead — queue on-demand range request.
        shared.request_range(8000..9000);

        // Downloader picks up the demand.
        let demand = dl.poll_demand().await;
        assert!(demand.is_some(), "demand should be available");

        // After demand during Sequential, plan should NOT return Step.
        let outcome = dl.plan().await;
        assert!(
            !matches!(outcome, PlanOutcome::Step),
            "sequential should be cancelled after demand — plan must not return Step"
        );
    }
}
