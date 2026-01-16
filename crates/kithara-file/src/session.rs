use std::{ops::Range, pin::Pin, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_assets::{
    AssetId, AssetResource, AssetStore, CachedAssets, DiskAssetStore, EvictAssets, LeaseGuard,
};
use kithara_net::{HttpClient, NetError};
use kithara_storage::{StreamingResource, StreamingResourceExt};
use kithara_stream::{
    EngineHandle, Source, StreamError as KitharaIoError, StreamMsg,
    StreamResult as KitharaIoResult, WaitOutcome, Writer,
};
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;
use tracing::trace;
use url::Url;

use crate::{
    driver::{DriverError, FileDriver, FileStreamState, SourceError},
    events::FileEvent,
};

// Type aliases for complex types
type AssetResourceType =
    AssetResource<StreamingResource, LeaseGuard<CachedAssets<EvictAssets<DiskAssetStore>>>>;

/// Progress tracker for download and playback positions.
#[derive(Debug)]
pub struct Progress {
    read_pos: std::sync::atomic::AtomicU64,
    download_pos: std::sync::atomic::AtomicU64,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            read_pos: std::sync::atomic::AtomicU64::new(0),
            download_pos: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn set_read_pos(&self, v: u64) {
        use std::sync::atomic::Ordering;
        self.read_pos.store(v, Ordering::Relaxed);
    }

    pub fn set_download_pos(&self, v: u64) {
        use std::sync::atomic::Ordering;
        self.download_pos.store(v, Ordering::Relaxed);
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SessionSource {
    res: AssetResourceType,
    progress: Arc<Progress>,
    events: broadcast::Sender<FileEvent>,
    len: Option<u64>,
}

impl SessionSource {
    /// Create a new session source.
    pub(crate) fn new(
        res: AssetResourceType,
        progress: Arc<Progress>,
        events: broadcast::Sender<FileEvent>,
        len: Option<u64>,
    ) -> Self {
        Self {
            res,
            progress,
            events,
            len,
        }
    }

    pub fn events(&self) -> broadcast::Receiver<FileEvent> {
        self.events.subscribe()
    }
}

#[async_trait]
impl Source for SessionSource {
    type Error = SourceError;

    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome, Self::Error> {
        trace!(
            start = range.start,
            end = range.end,
            "kithara-file SessionSource wait_range begin"
        );

        match self
            .res
            .wait_range(range)
            .await
            .map_err(|e| KitharaIoError::Source(SourceError::Storage(e)))?
        {
            kithara_storage::WaitOutcome::Ready => {
                trace!("kithara-file SessionSource wait_range -> Ready");
                Ok(WaitOutcome::Ready)
            }
            kithara_storage::WaitOutcome::Eof => {
                trace!("kithara-file SessionSource wait_range -> Eof");
                Ok(WaitOutcome::Eof)
            }
        }
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> KitharaIoResult<usize, Self::Error> {
        trace!(
            offset,
            len = buf.len(),
            "kithara-file SessionSource read_at begin"
        );
        let bytes_read = self
            .res
            .read_at(offset, buf)
            .await
            .map_err(|e| KitharaIoError::Source(SourceError::Storage(e)))?;

        let new_pos = offset.saturating_add(bytes_read as u64);
        self.progress.set_read_pos(new_pos);
        let percent = self
            .len
            .map(|len| ((new_pos as f64 / len as f64) * 100.0).min(100.0) as f32);
        let _ = self.events.send(FileEvent::PlaybackProgress {
            position: new_pos,
            percent,
        });

        trace!(
            offset,
            requested = buf.len(),
            got = bytes_read,
            "kithara-file SessionSource read_at done"
        );
        Ok(bytes_read)
    }

    fn len(&self) -> Option<u64> {
        self.len
    }
}

pub struct FileSession {
    driver: Arc<FileDriver>,
    net_client: HttpClient,
    engine_handle: Arc<Mutex<Option<EngineHandle>>>,
}

impl FileSession {
    pub fn new(
        asset_id: AssetId,
        url: Url,
        net_client: HttpClient,
        assets: Arc<AssetStore>,
        cancel: CancellationToken,
        event_capacity: usize,
    ) -> Self {
        let driver = Arc::new(FileDriver::new(
            asset_id,
            url,
            net_client.clone(),
            assets,
            cancel,
            event_capacity,
        ));

        Self {
            driver,
            net_client,
            engine_handle: Arc::new(Mutex::new(None)),
        }
    }

    pub fn asset_id(&self) -> AssetId {
        self.driver.asset_id()
    }

    pub async fn stream(
        &self,
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes, FileError>> + Send + '_>> {
        let (handle, driver_stream) = self.driver.stream_with_handle().await;
        {
            let mut guard = self.engine_handle.lock().await;
            *guard = Some(handle);
        }
        Box::pin(driver_stream.map(|result| result.map_err(FileError::Driver)))
    }

    pub async fn source(&self) -> Result<SessionSource, FileError> {
        let driver = &self.driver;

        let state = FileStreamState::create(
            driver.assets(),
            self.net_client.clone(),
            driver.url().clone(),
            driver.cancel().clone(),
            driver.event_capacity(),
        )
        .await
        .map_err(|e| FileError::Driver(DriverError::Source(e)))?;

        let progress = Arc::new(Progress::new());
        Self::spawn_download_writer(&self.net_client, state.clone(), progress.clone());

        Ok(SessionSource::new(
            state.res().clone(),
            progress,
            state.events().clone(),
            state.len(),
        ))
    }

    fn spawn_download_writer(
        net_client: &HttpClient,
        state: Arc<FileStreamState>,
        progress: Arc<Progress>,
    ) {
        Self::spawn_download_writer_static(net_client, state, progress);
    }

    /// Static version of spawn_download_writer for use without FileSession.
    pub(crate) fn spawn_download_writer_static(
        net_client: &HttpClient,
        state: Arc<FileStreamState>,
        progress: Arc<Progress>,
    ) {
        let net = net_client.clone();
        let url = state.url().clone();
        let events = state.events().clone();
        let len = state.len();
        let res = state.res().clone();
        let cancel = state.cancel().clone();
        let progress_dl = progress.clone();

        tokio::spawn(async move {
            let writer = Writer::<_, _, FileEvent>::new(net, url, None, res, cancel).with_event(
                move |offset, _len| {
                    progress_dl.set_download_pos(offset);
                    let percent =
                        len.map(|len| ((offset as f64 / len as f64) * 100.0).min(100.0) as f32);
                    FileEvent::DownloadProgress { offset, percent }
                },
                move |msg| {
                    if let StreamMsg::Event(ev) = msg {
                        let _ = events.send(ev);
                    }
                },
            );

            let _ = writer.run_with_fail().await;
        });
    }

    pub async fn seek_bytes(&self, position: u64) -> FileResult<()> {
        let handle = {
            let guard = self.engine_handle.lock().await;
            guard.clone()
        };
        let Some(handle) = handle else {
            return Err(FileError::Driver(DriverError::SeekNotSupported));
        };

        handle
            .seek_bytes::<SourceError>(position)
            .await
            .map_err(DriverError::from)
            .map_err(FileError::Driver)
    }
}

impl Clone for FileSession {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            net_client: self.net_client.clone(),
            engine_handle: self.engine_handle.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("Driver error: {0}")]
    Driver(#[from] DriverError),
    #[error("Driver has stopped")]
    DriverStopped,
    #[error("Network error: {0}")]
    Net(#[from] NetError),
    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),
}

pub type FileResult<T> = Result<T, FileError>;
