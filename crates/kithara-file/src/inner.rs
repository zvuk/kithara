//! File stream type implementation.
//!
//! Provides `File` marker type implementing `StreamType` trait
//! and `FileDownloader` implementing `Downloader` trait.

use std::sync::Arc;

use futures::StreamExt;
use kithara_assets::{
    AssetStoreBuilder, Assets, CoverageIndex, DiskCoverage, ResourceKey, asset_root_for_url,
};
use kithara_net::{Headers, HttpClient};
use kithara_storage::{Coverage, MemCoverage, MmapResource, ResourceExt, ResourceStatus};
use kithara_stream::{
    Backend, Downloader, DownloaderIo, PlanOutcome, StepResult, StreamType, Writer, WriterItem,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    config::{FileConfig, FileSrc},
    error::SourceError,
    events::FileEvent,
    session::{FileSource, FileStreamState, Progress, SharedFileState},
};

/// Marker type for file streaming.
pub struct File;

impl StreamType for File {
    type Config = FileConfig;
    type Source = FileSource;
    type Error = SourceError;
    type Event = FileEvent;

    fn ensure_events(config: &mut Self::Config) -> broadcast::Receiver<Self::Event> {
        if config.events_tx.is_none() {
            let capacity = config.events_channel_capacity.max(1);
            config.events_tx = Some(broadcast::channel(capacity).0);
        }
        match config.events_tx {
            Some(ref tx) => tx.subscribe(),
            None => broadcast::channel(1).1,
        }
    }

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        let cancel = config.cancel.clone().unwrap_or_default();
        let src = config.src.clone();

        match src {
            FileSrc::Local(path) => Self::create_local(path, config, cancel),
            FileSrc::Remote(url) => Self::create_remote(url, config, cancel).await,
        }
    }
}

impl File {
    /// Create a source for a local file.
    ///
    /// Opens the file via `AssetStore` with an absolute `ResourceKey`,
    /// skipping network and background downloader entirely.
    fn create_local(
        path: std::path::PathBuf,
        config: FileConfig,
        cancel: CancellationToken,
    ) -> Result<FileSource, SourceError> {
        if !path.exists() {
            return Err(SourceError::InvalidPath(format!(
                "file not found: {}",
                path.display()
            )));
        }

        let store = AssetStoreBuilder::new()
            .asset_root(None)
            .cache_enabled(false)
            .lease_enabled(false)
            .evict_enabled(false)
            .cancel(cancel)
            .build();

        let key = ResourceKey::absolute(path);
        let res = store.open_resource(&key).map_err(SourceError::Assets)?;
        let len = res.len();

        let events = config
            .events_tx
            .unwrap_or_else(|| broadcast::channel(config.events_channel_capacity.max(1)).0);

        let progress = Arc::new(Progress::new());
        // Local file is fully available — mark download as complete.
        let total = len.unwrap_or(0);
        progress.set_download_pos(total);
        let _ = events.send(FileEvent::DownloadComplete { total_bytes: total });

        Ok(FileSource::new(res, progress, events, len))
    }

    /// Create a source for a remote file (HTTP/HTTPS).
    async fn create_remote(
        url: url::Url,
        config: FileConfig,
        cancel: CancellationToken,
    ) -> Result<FileSource, SourceError> {
        let asset_root = asset_root_for_url(&url, config.name.as_deref());

        let backend = AssetStoreBuilder::new()
            .root_dir(&config.store.cache_dir)
            .asset_root(Some(asset_root.as_str()))
            .evict_config(config.store.to_evict_config())
            .cancel(cancel.clone())
            .build();

        // Open coverage index for crash-safe download tracking.
        // Only available for disk-backed storage (ephemeral/mem doesn't need it).
        let coverage_index = match &backend {
            kithara_assets::AssetsBackend::Disk(store) => store
                .open_coverage_index_resource()
                .ok()
                .map(|res| Arc::new(CoverageIndex::new(res))),
            kithara_assets::AssetsBackend::Mem(_) => None,
        };

        let net_client = HttpClient::new(config.net.clone());

        let state = FileStreamState::create(
            Arc::new(backend),
            &net_client,
            url,
            cancel.clone(),
            config.events_tx.clone(),
            config.events_channel_capacity,
        )
        .await?;

        let progress = Arc::new(Progress::new());
        let shared = Arc::new(SharedFileState::new());

        // Determine if the resource is a complete cache or needs downloading.
        let is_partial = match state.res().status() {
            ResourceStatus::Committed { final_len } => {
                // File on disk might be smaller than HEAD Content-Length → partial.
                state
                    .len()
                    .zip(final_len)
                    .is_some_and(|(expected, actual)| actual < expected)
            }
            _ => false,
        };

        if matches!(state.res().status(), ResourceStatus::Committed { .. }) && !is_partial {
            // Fully cached — no download needed.
            tracing::debug!("file already cached, skipping download");
            let total = state.len().unwrap_or(0);
            progress.set_download_pos(total);
            let _ = state
                .events()
                .send(FileEvent::DownloadComplete { total_bytes: total });
        } else {
            if is_partial {
                // Partial cache: reactivate resource for continued writing.
                tracing::debug!("partial cache detected, reactivating for on-demand download");
                state.res().reactivate().map_err(SourceError::Storage)?;
            }

            // Create downloader with shared state for on-demand loading.
            // For partial cache, downloader starts in on-demand-only mode
            // (sequential stream from scratch, but partial data already on disk).
            let downloader = FileDownloader::new(
                &net_client,
                state.clone(),
                progress.clone(),
                state.events().clone(),
                config.look_ahead_bytes,
                shared.clone(),
                coverage_index,
            )
            .await;

            // Spawn downloader on the thread pool.
            // Backend is stored in FileSource — dropping the source cancels the downloader.
            let backend = Backend::new(downloader, &cancel, &config.thread_pool);

            // Create source with shared state and backend for on-demand loading.
            let source = FileSource::with_shared(
                state.res().clone(),
                progress,
                state.events().clone(),
                state.len(),
                shared,
                backend,
            );

            return Ok(source);
        }

        // Fully cached — create source without backend (no downloader needed).
        let source = FileSource::new(
            state.res().clone(),
            progress,
            state.events().clone(),
            state.len(),
        );

        Ok(source)
    }
}

// FileIo — pure I/O executor (Clone, no &mut self)

/// Pure I/O executor for file range fetching.
#[derive(Clone)]
pub struct FileIo {
    net_client: HttpClient,
    url: url::Url,
    res: kithara_assets::AssetResource,
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

    async fn fetch(&self, plan: FilePlan) -> Result<FileFetch, FileDownloadError> {
        let range = plan.range;
        let range_header = format!("bytes={}-{}", range.start, range.end.saturating_sub(1));
        let mut headers = Headers::new();
        headers.insert("Range", range_header);

        match self
            .net_client
            .stream(self.url.clone(), Some(headers))
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

// FileDownloader — mutable state (plan + commit)

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
    res: kithara_assets::AssetResource,
    progress: Arc<Progress>,
    events_tx: broadcast::Sender<FileEvent>,
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
    async fn new(
        net_client: &HttpClient,
        state: Arc<FileStreamState>,
        progress: Arc<Progress>,
        events_tx: broadcast::Sender<FileEvent>,
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
                let _ = events_tx.send(FileEvent::DownloadError {
                    error: e.to_string(),
                });
                // Return a writer from an empty stream so step() returns error immediately
                let empty = futures::stream::empty();
                let boxed: kithara_net::ByteStream = Box::pin(empty);
                Writer::new(boxed, state.res().clone(), CancellationToken::new())
            }
        };

        let mut coverage: FileCoverage = match coverage_index {
            Some(idx) => {
                let key = url.to_string();
                let mut dc = DiskCoverage::open(idx, key);
                if let Some(size) = total {
                    dc.set_total_size(size);
                }
                FileCoverage::Disk(dc)
            }
            None => {
                let mc = if let Some(size) = total {
                    MemCoverage::with_total_size(size)
                } else {
                    MemCoverage::new()
                };
                FileCoverage::Memory(mc)
            }
        };

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
            events_tx,
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
                            self.res.fail(format!("commit failed: {}", e));
                            let _ = self.events_tx.send(FileEvent::DownloadError {
                                error: format!("commit failed: {}", e),
                            });
                        } else if let Some(total) = self.total {
                            let _ = self
                                .events_tx
                                .send(FileEvent::DownloadComplete { total_bytes: total });
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
                let _ = self.events_tx.send(FileEvent::DownloadError {
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
                let _ = self.events_tx.send(FileEvent::DownloadProgress {
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
                        self.res.fail(format!("commit failed: {}", e));
                        let _ = self.events_tx.send(FileEvent::DownloadError {
                            error: format!("commit failed: {}", e),
                        });
                    } else if let Some(total) = self.total {
                        let _ = self
                            .events_tx
                            .send(FileEvent::DownloadComplete { total_bytes: total });
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
                        self.res.fail(format!("commit failed: {}", e));
                        let _ = self.events_tx.send(FileEvent::DownloadError {
                            error: format!("commit failed: {}", e),
                        });
                    } else {
                        let _ = self
                            .events_tx
                            .send(FileEvent::DownloadComplete { total_bytes });
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
                    let _ = self.events_tx.send(FileEvent::DownloadProgress {
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
}
