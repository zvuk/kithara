//! Background file downloader and I/O executor.
//!
//! Contains `FileDownloader` implementing `Downloader` trait,
//! `FileIo` implementing `DownloaderIo`, and supporting types.

use std::{error::Error, fmt, future::Future, ops::Range, sync::Arc};

use futures::StreamExt;
use kithara_assets::AssetResource;
use kithara_events::{EventBus, FileEvent};
use kithara_net::{Headers, HttpClient, RangeSpec};
use kithara_platform::{WasmSend, tokio};
use kithara_storage::ResourceExt;
use kithara_stream::{Downloader, DownloaderIo, PlanOutcome, StepResult, Writer, WriterItem};
use tokio_util::sync::CancellationToken;

use crate::session::{FileStreamState, Progress, SharedFileState};

// FileIo — pure I/O executor (Clone, no &mut self)

/// Pure I/O executor for file range fetching.
#[derive(Clone)]
pub(crate) struct FileIo {
    net_client: HttpClient,
    url: url::Url,
    res: AssetResource,
    cancel: CancellationToken,
    headers: Option<Headers>,
}

/// Plan for downloading a file range.
pub(crate) struct FilePlan {
    pub(crate) range: Range<u64>,
}

/// Result of a file download operation.
pub(crate) enum FileFetch {
    /// A chunk was written during sequential streaming.
    Chunk { offset: u64, len: u64 },
    /// A range request completed.
    RangeDone,
    /// Sequential stream ended.
    StreamEnded { total_bytes: u64 },
}

/// File download error.
#[derive(Debug)]
pub(crate) struct FileDownloadError {
    msg: String,
}

impl fmt::Display for FileDownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "file download error: {}", self.msg)
    }
}

impl Error for FileDownloadError {}

impl DownloaderIo for FileIo {
    type Plan = FilePlan;
    type Fetch = FileFetch;
    type Error = FileDownloadError;

    async fn fetch(&self, plan: Self::Plan) -> Result<Self::Fetch, Self::Error> {
        let range = plan.range;
        let spec = RangeSpec::new(range.start, Some(range.end.saturating_sub(1)));

        match self
            .net_client
            .get_range(self.url.clone(), spec, self.headers.clone())
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
                Ok(FileFetch::RangeDone)
            }
            Err(e) => {
                tracing::warn!(?e, "failed to start range request");
                Err(FileDownloadError { msg: e.to_string() })
            }
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
///
/// The sequential `Writer` is created lazily on the first `step()` call,
/// matching the HLS pattern where I/O is deferred to the download loop.
/// This keeps `FileDownloader` `Send` on WASM (no `!Send` JS objects
/// in the struct) so the downloader can run in a Web Worker.
pub(crate) struct FileDownloader {
    io: FileIo,
    /// Sequential writer — created lazily on first `step()`.
    /// Wrapped in `WasmSend` because `Writer` is `!Send` on WASM (contains
    /// `JsValue`-backed response stream). The writer is always `None` when
    /// the struct is moved to a Web Worker, and only populated inside the
    /// worker via `ensure_writer()`.
    writer: WasmSend<Option<Writer>>,
    res: AssetResource,
    progress: Arc<Progress>,
    bus: EventBus,
    total: Option<u64>,
    /// Backpressure threshold. None = no backpressure.
    look_ahead_bytes: Option<u64>,
    shared: Arc<SharedFileState>,
    /// Current download phase.
    phase: FilePhase,
    /// Start offset for restarted sequential download (backward seek).
    /// When set, `ensure_writer()` opens a Range GET from this offset.
    sequential_offset: Option<u64>,
}

impl FileDownloader {
    pub(crate) fn new(
        net_client: &HttpClient,
        state: &Arc<FileStreamState>,
        progress: Arc<Progress>,
        bus: EventBus,
        look_ahead_bytes: Option<u64>,
        shared: Arc<SharedFileState>,
    ) -> Self {
        let url = state.url().clone();
        let total = state.len();
        let res = state.res().clone();
        let cancel = state.cancel().clone();
        let req_headers = state.headers().cloned();

        let io = FileIo {
            net_client: net_client.clone(),
            url,
            res: res.clone(),
            cancel,
            headers: req_headers,
        };

        Self {
            io,
            writer: WasmSend::new(None), // Created lazily on first step()
            res,
            progress,
            bus,
            total,
            look_ahead_bytes,
            shared,
            phase: FilePhase::Sequential,
            sequential_offset: None,
        }
    }

    /// Set a pre-opened Writer from an already-opened HTTP stream.
    ///
    /// Used by `File::create_remote()` to pass the streaming GET connection
    /// directly to the downloader, avoiding a second HTTP request.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn with_initial_writer(mut self, writer: Writer) -> Self {
        *self.writer.get_mut() = Some(writer);
        self
    }

    /// Create or return the sequential Writer.
    ///
    /// When a pre-opened writer was set via `with_initial_writer()`, returns
    /// it directly. Otherwise falls back to opening a new HTTP stream.
    ///
    /// If `sequential_offset` is set (backward seek restart), opens a Range
    /// GET from that offset instead of streaming from the beginning.
    async fn ensure_writer(&mut self) -> Result<&mut Writer, FileDownloadError> {
        let slot = self.writer.get_mut();
        if slot.is_none() {
            let start_offset = self.sequential_offset.take().unwrap_or(0);
            let result = if start_offset > 0 {
                // Backward seek restart: open Range GET from the seek position.
                let spec = RangeSpec::from_start(start_offset);
                self.io
                    .net_client
                    .get_range(self.io.url.clone(), spec, self.io.headers.clone())
                    .await
            } else {
                self.io
                    .net_client
                    .stream(self.io.url.clone(), self.io.headers.clone())
                    .await
            };

            match result {
                Ok(byte_stream) => {
                    // Discover content-length from response headers if unknown
                    // (fallback path — normally set by create_remote via
                    // with_initial_writer before we reach here).
                    if self.total.is_none()
                        && let Some(cl) = byte_stream
                            .headers
                            .get("content-length")
                            .or_else(|| byte_stream.headers.get("Content-Length"))
                            .and_then(|v| v.parse::<u64>().ok())
                    {
                        tracing::debug!(
                            content_length = cl,
                            "discovered total from stream headers"
                        );
                        self.total = Some(cl);
                        self.progress.timeline().set_total_bytes(Some(cl));
                    }

                    *slot = Some(Writer::with_offset(
                        byte_stream,
                        self.res.clone(),
                        self.io.cancel.clone(),
                        start_offset,
                    ));
                }
                Err(e) => {
                    tracing::warn!("failed to open stream: {}", e);
                    self.res.fail(e.to_string());
                    self.bus.publish(FileEvent::DownloadError {
                        error: e.to_string(),
                    });
                    return Err(FileDownloadError { msg: e.to_string() });
                }
            }
        }
        // writer was just set to Some in the if-block above,
        // or was pre-populated via with_initial_writer().
        slot.as_mut().ok_or_else(|| FileDownloadError {
            msg: "writer initialization failed".to_string(),
        })
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
            // Drop the sequential writer to release the HTTP connection.
            *self.writer.get_mut() = None;
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
                let total = self.total.unwrap_or(0);

                let mut gap_cursor: u64 = 0;
                for _ in 0..gap_batch_size {
                    let Some(gap) = self.res.next_gap(gap_cursor, total) else {
                        break;
                    };
                    let clamped_end = (gap.start + gap_chunk_size).min(gap.end);
                    plans.push(FilePlan {
                        range: gap.start..clamped_end,
                    });
                    gap_cursor = clamped_end;
                }

                if plans.is_empty() {
                    // No more gaps — check if complete.
                    if self.res.next_gap(0, total).is_none() && total > 0 {
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

        // Lazily open the HTTP stream on first step.
        let writer = self.ensure_writer().await?;

        // Sequential download continues.
        let Some(result) = writer.next().await else {
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
                // write_at already updates Resource.available — no coverage mark needed.
                self.progress.set_download_pos(download_offset);
                self.bus.publish(FileEvent::DownloadProgress {
                    offset: download_offset,
                    total: self.total,
                });
            }
            FileFetch::RangeDone => {
                // Check if gap-filling completed everything.
                let total = self.total.unwrap_or(0);
                if total > 0 && self.res.next_gap(0, total).is_none() {
                    // Only commit + emit event on first completion.
                    if !matches!(self.phase, FilePhase::Complete) {
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
                    }
                    self.phase = FilePhase::Complete;
                }
            }
            FileFetch::StreamEnded { total_bytes } => {
                if self.total.is_none() {
                    self.total = Some(total_bytes);
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

    #[cfg_attr(feature = "perf", hotpath::measure)]
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

    fn wait_ready(&self) -> impl Future<Output = ()> {
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

    fn demand_signal(&self) -> impl Future<Output = ()> + use<> {
        let shared = Arc::clone(&self.shared);
        async move {
            shared.reader_needs_data.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_assets::{AssetStoreBuilder, ResourceKey};
    use kithara_net::{HttpClient, NetOptions};
    use kithara_stream::Timeline;
    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// After a demand (on-demand range request) is polled during Sequential phase,
    /// the downloader should cancel sequential and switch to gap-filling.
    ///
    /// Currently FAILS: `poll_demand` does not change phase, so `plan()` still returns
    /// Step and the sequential download continues wasting bandwidth.
    #[kithara::test(tokio)]
    async fn sequential_stops_after_demand() {
        let cancel = CancellationToken::new();

        let store = AssetStoreBuilder::new()
            .ephemeral(true)
            .asset_root(Some("test"))
            .cancel(cancel.clone())
            .build();

        let url: url::Url = "http://example.com/test.mp3".parse().unwrap();
        let key = ResourceKey::from_url(&url);
        let res = store.open_resource(&key).unwrap();

        let total: u64 = 10_000;

        let io = FileIo {
            net_client: HttpClient::new(NetOptions::default()),
            url: url.clone(),
            res: res.clone(),
            cancel: cancel.clone(),
            headers: None,
        };

        let shared = Arc::new(SharedFileState::new());

        // Write first 1KB to simulate sequential download progress.
        res.write_at(0, &[0u8; 1000]).unwrap();

        // Pending stream simulates a stalled sequential download.
        let stream = futures::stream::pending::<Result<bytes::Bytes, kithara_net::NetError>>();
        let writer = Writer::new(stream, res.clone(), cancel.clone());

        let mut dl = FileDownloader {
            io,
            writer: WasmSend::new(Some(writer)),
            res,
            progress: Arc::new(Progress::new(Timeline::new())),
            bus: EventBus::new(16),
            total: Some(total),
            look_ahead_bytes: None,
            shared: shared.clone(),
            phase: FilePhase::Sequential,
            sequential_offset: None,
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
