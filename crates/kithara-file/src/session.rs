#![forbid(unsafe_code)]

use std::{ops::Range, sync::Arc};

use crossbeam_queue::SegQueue;
use kithara_assets::{AssetResource, AssetsBackend, ResourceKey};
use kithara_events::{EventBus, FileEvent};
use kithara_net::{Headers, HttpClient, Net};
use kithara_storage::{ResourceExt, ResourceStatus, WaitOutcome};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::error::SourceError;

#[derive(Debug, Clone)]
pub(crate) struct FileStreamState {
    pub(crate) url: Url,
    pub(crate) cancel: CancellationToken,
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) headers: Option<Headers>,
    pub(crate) len: Option<u64>,
}

impl FileStreamState {
    pub(crate) async fn create(
        assets: Arc<AssetsBackend>,
        net_client: &HttpClient,
        url: Url,
        cancel: CancellationToken,
        bus: Option<EventBus>,
        event_channel_capacity: usize,
        headers: Option<Headers>,
    ) -> Result<Arc<Self>, SourceError> {
        let resp_headers = net_client.head(url.clone(), headers.clone()).await?;
        let len = resp_headers
            .get("content-length")
            .or_else(|| resp_headers.get("Content-Length"))
            .and_then(|v| v.parse::<u64>().ok());

        let key = ResourceKey::from_url(&url);
        let res = assets.open_resource(&key).map_err(SourceError::Assets)?;

        let bus = bus.unwrap_or_else(|| EventBus::new(event_channel_capacity));

        Ok(Arc::new(Self {
            url,
            cancel,
            res,
            bus,
            headers,
            len,
        }))
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn res(&self) -> &AssetResource {
        &self.res
    }

    pub(crate) fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub(crate) fn headers(&self) -> Option<&Headers> {
        self.headers.as_ref()
    }

    pub(crate) fn len(&self) -> Option<u64> {
        self.len
    }

    pub(crate) fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }
}

/// Progress tracker for download and playback positions.
pub(crate) struct Progress {
    read_pos: std::sync::atomic::AtomicU64,
    download_pos: std::sync::atomic::AtomicU64,
    /// Source -> Downloader: reader advanced, may resume downloading.
    reader_advanced: Notify,
}

impl Progress {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            read_pos: std::sync::atomic::AtomicU64::new(0),
            download_pos: std::sync::atomic::AtomicU64::new(0),
            reader_advanced: Notify::new(),
        }
    }

    pub(crate) fn read_pos(&self) -> u64 {
        use std::sync::atomic::Ordering;
        self.read_pos.load(Ordering::Acquire)
    }

    pub(crate) fn download_pos(&self) -> u64 {
        use std::sync::atomic::Ordering;
        self.download_pos.load(Ordering::Acquire)
    }

    pub(crate) fn set_read_pos(&self, v: u64) {
        use std::sync::atomic::Ordering;
        self.read_pos.store(v, Ordering::Release);
        self.reader_advanced.notify_one();
    }

    pub(crate) fn set_download_pos(&self, v: u64) {
        use std::sync::atomic::Ordering;
        self.download_pos.store(v, Ordering::Release);
    }

    /// Register for reader advance notification.
    /// Must be called BEFORE checking positions to avoid race.
    pub(crate) fn notified_reader_advance(&self) -> tokio::sync::futures::Notified<'_> {
        self.reader_advanced.notified()
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared state between `FileSource` (sync reader) and `FileDownloader` (async).
///
/// Enables on-demand Range requests: when the reader needs data beyond what
/// the sequential download has fetched, it pushes a range request here.
/// The downloader picks it up and fetches via HTTP Range.
///
/// Data availability signaling relies on `StorageResource`'s internal condvar:
/// when the downloader writes data via `write_at`, the resource notifies waiters,
/// so `FileSource::wait_range()` unblocks automatically.
#[derive(Debug)]
pub(crate) struct SharedFileState {
    /// Queue of on-demand range requests from Source to Downloader.
    range_requests: SegQueue<Range<u64>>,
    /// Notify from Source → Downloader when reader needs data.
    pub(crate) reader_needs_data: Notify,
}

impl SharedFileState {
    pub(crate) fn new() -> Self {
        Self {
            range_requests: SegQueue::new(),
            reader_needs_data: Notify::new(),
        }
    }

    /// Request on-demand download of a range.
    pub(crate) fn request_range(&self, range: Range<u64>) {
        self.range_requests.push(range);
        self.reader_needs_data.notify_one();
    }

    /// Pop next on-demand range request (for Downloader).
    pub(crate) fn pop_range_request(&self) -> Option<Range<u64>> {
        self.range_requests.pop()
    }
}

/// File source implementing Source trait (sync).
///
/// Wraps storage resource with progress tracking and event emission.
/// Holds an optional [`Backend`](kithara_stream::Backend) to manage the
/// downloader lifecycle: when this source is dropped, the backend is dropped,
/// cancelling the downloader task automatically.
pub struct FileSource {
    res: AssetResource,
    progress: Arc<Progress>,
    bus: EventBus,
    len: Option<u64>,
    shared: Option<Arc<SharedFileState>>,
    /// Downloader backend. Dropped with this source, cancelling the downloader.
    _backend: Option<kithara_stream::Backend>,
}

impl FileSource {
    /// Create new file source (no on-demand support — for local files).
    pub(crate) fn new(
        res: AssetResource,
        progress: Arc<Progress>,
        bus: EventBus,
        len: Option<u64>,
    ) -> Self {
        Self {
            res,
            progress,
            bus,
            len,
            shared: None,
            _backend: None,
        }
    }

    /// Create new file source with shared state and backend (for remote files).
    pub(crate) fn with_shared(
        res: AssetResource,
        progress: Arc<Progress>,
        bus: EventBus,
        len: Option<u64>,
        shared: Arc<SharedFileState>,
        backend: kithara_stream::Backend,
    ) -> Self {
        Self {
            res,
            progress,
            bus,
            len,
            shared: Some(shared),
            _backend: Some(backend),
        }
    }
}

impl kithara_stream::Source for FileSource {
    type Error = SourceError;

    fn wait_range(
        &mut self,
        range: Range<u64>,
    ) -> kithara_stream::StreamResult<WaitOutcome, SourceError> {
        use kithara_stream::StreamError;

        // Update read position so downloader knows where reader needs data.
        // This prevents backpressure deadlock when symphonia seeks ahead
        // (e.g., SeekFrom::End) while downloader is still near the beginning.
        if range.start > self.progress.read_pos() {
            self.progress.set_read_pos(range.start);
        }

        // If shared state exists, check if on-demand download is needed
        // BEFORE calling res.wait_range() — the resource blocks indefinitely
        // for Active resources when data is not available.
        if let Some(ref shared) = self.shared {
            let download_pos = self.progress.download_pos();
            if range.start >= download_pos
                && !matches!(self.res.status(), ResourceStatus::Committed { .. })
            {
                debug!(
                    range_start = range.start,
                    range_end = range.end,
                    download_pos,
                    "requesting on-demand download for range beyond download_pos"
                );
                shared.request_range(range.clone());
            }
        }

        // Wait on the resource. If on-demand was requested, the downloader
        // will write data via write_at, which notifies the resource's condvar
        // and unblocks this call.
        self.res
            .wait_range(range)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))
    }

    fn read_at(
        &mut self,
        offset: u64,
        buf: &mut [u8],
    ) -> kithara_stream::StreamResult<usize, SourceError> {
        use kithara_stream::StreamError;

        let n = self
            .res
            .read_at(offset, buf)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))?;

        if n > 0 {
            // Update progress
            let new_pos = offset.saturating_add(n as u64);
            self.progress.set_read_pos(new_pos);

            self.bus.publish(FileEvent::PlaybackProgress {
                position: new_pos,
                total: self.len,
            });

            trace!(offset, bytes = n, "FileSource read complete");
        }

        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        // For partial downloads, return expected total (from Content-Length HEAD)
        // so decoder can calculate correct duration and seek bounds.
        // For committed files, resource.len() == expected total anyway.
        self.len.or_else(|| self.res.len())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_assets::{AssetStoreBuilder, ResourceKey};
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    // Progress

    #[test]
    fn test_progress_initial_state() {
        let p = Progress::new();
        assert_eq!(p.read_pos(), 0);
        assert_eq!(p.download_pos(), 0);
    }

    #[test]
    fn test_progress_set_and_get_read_pos() {
        let p = Progress::new();
        p.set_read_pos(100);
        assert_eq!(p.read_pos(), 100);
    }

    #[test]
    fn test_progress_set_and_get_download_pos() {
        let p = Progress::new();
        p.set_download_pos(500);
        assert_eq!(p.download_pos(), 500);
    }

    #[tokio::test]
    async fn test_progress_signal_reader_advanced() {
        let progress = Arc::new(Progress::new());

        // Register the notified future BEFORE signaling (Notify protocol).
        let notified = progress.notified_reader_advance();

        // Spawn a task that will signal after a short delay.
        let p = Arc::clone(&progress);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            p.set_read_pos(42);
        });

        // The notified future must resolve within the timeout.
        tokio::time::timeout(std::time::Duration::from_secs(2), notified)
            .await
            .unwrap();

        assert_eq!(progress.read_pos(), 42);
    }

    // SharedFileState

    #[test]
    fn test_shared_file_state_empty_queue() {
        let state = SharedFileState::new();
        assert!(state.pop_range_request().is_none());
    }

    #[test]
    fn test_shared_file_state_request_and_pop() {
        let state = SharedFileState::new();
        state.request_range(100..200);
        let r = state.pop_range_request().unwrap();
        assert_eq!(r, 100..200);
    }

    #[test]
    fn test_shared_file_state_fifo_order() {
        let state = SharedFileState::new();
        state.request_range(0..10);
        state.request_range(10..20);
        state.request_range(20..30);

        assert_eq!(state.pop_range_request().unwrap(), 0..10);
        assert_eq!(state.pop_range_request().unwrap(), 10..20);
        assert_eq!(state.pop_range_request().unwrap(), 20..30);
    }

    #[test]
    fn test_shared_file_state_multiple_pops() {
        let state = SharedFileState::new();
        state.request_range(0..50);
        state.request_range(50..100);

        assert!(state.pop_range_request().is_some());
        assert!(state.pop_range_request().is_some());
        assert!(state.pop_range_request().is_none());
    }

    // FileSource

    /// Helper: create a committed `AssetResource` backed by a file with `data`.
    fn create_committed_resource(dir: &TempDir, data: &[u8]) -> AssetResource {
        let file_path = dir.path().join("test.dat");
        std::fs::write(&file_path, data).unwrap();

        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(None)
            .cache_enabled(false)
            .lease_enabled(false)
            .evict_enabled(false)
            .cancel(CancellationToken::new())
            .build();

        let key = ResourceKey::absolute(&file_path);
        store.open_resource(&key).unwrap()
    }

    #[test]
    fn test_file_source_read_at() {
        use kithara_stream::Source;

        let dir = TempDir::new().unwrap();
        let data = b"hello world from kithara";
        let res = create_committed_resource(&dir, data);

        let progress = Arc::new(Progress::new());
        let bus = EventBus::new(16);

        let mut source = FileSource::new(res, Arc::clone(&progress), bus, Some(data.len() as u64));

        let mut buf = [0u8; 11];
        let n = source.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf[..n], b"hello world");

        // Progress should be updated to offset + bytes_read.
        assert_eq!(progress.read_pos(), 11);

        // Read from an offset.
        let mut buf2 = [0u8; 7];
        let n2 = source.read_at(6, &mut buf2).unwrap();
        assert_eq!(n2, 7);
        assert_eq!(&buf2[..n2], b"world f");
        assert_eq!(progress.read_pos(), 13);
    }

    #[test]
    fn test_file_source_len() {
        let dir = TempDir::new().unwrap();
        let res = create_committed_resource(&dir, b"abc");

        let progress = Arc::new(Progress::new());
        let bus = EventBus::new(16);

        let source = FileSource::new(res, progress, bus, Some(12345));

        // len() should return the explicit value provided at construction.
        use kithara_stream::Source;
        assert_eq!(source.len(), Some(12345));
    }
}
