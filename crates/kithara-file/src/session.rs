use std::{ops::Range, pin::Pin, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt, stream::BoxStream};
use kithara_assets::{AssetResource, AssetStore, DiskAssetStore, EvictAssets, LeaseGuard};
use kithara_core::{AssetId, CoreError};
use kithara_io::{IoError as KitharaIoError, IoResult as KitharaIoResult, Source, WaitOutcome};
use kithara_net::{HttpClient, NetError};
use kithara_storage::{Resource, StreamingResource, StreamingResourceExt};
use kithara_stream::{Net, WriteSink, Writer};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::trace;
use url::Url;

use crate::{
    driver::{DriverError, FileCommand, FileDriver, SourceError},
    options::FileSourceOptions,
};

// Type aliases for complex types
type AssetResourceType = AssetResource<StreamingResource, LeaseGuard<EvictAssets<DiskAssetStore>>>;

#[derive(Debug)]
pub struct Progress {
    read_pos: std::sync::atomic::AtomicU64,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            read_pos: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn set_read_pos(&self, v: u64) {
        use std::sync::atomic::Ordering;
        self.read_pos.store(v, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct NetHttp(HttpClient);

impl Net for NetHttp {
    type Request = Url;
    type Error = SourceError;
    type ByteStream = BoxStream<'static, Result<Bytes, SourceError>>;

    fn stream(
        &self,
        req: Self::Request,
    ) -> futures::future::BoxFuture<'static, Result<Self::ByteStream, Self::Error>> {
        let client = self.0.clone();
        Box::pin(async move {
            let s = client.stream(req, None).await.map_err(SourceError::Net)?;
            Ok(s.map(|r| r.map_err(SourceError::Net)).boxed())
        })
    }
}

#[derive(Clone)]
struct AssetSink {
    res: AssetResourceType,
}

impl WriteSink for AssetSink {
    type Error = SourceError;

    fn write_at<'a>(
        &'a self,
        offset: u64,
        data: &'a [u8],
    ) -> futures::future::BoxFuture<'a, Result<(), Self::Error>> {
        let res = self.res.clone();
        Box::pin(async move {
            res.write_at(offset, data)
                .await
                .map_err(SourceError::Storage)
        })
    }

    fn commit<'a>(
        &'a self,
        final_len: Option<u64>,
    ) -> futures::future::BoxFuture<'a, Result<(), Self::Error>> {
        let res = self.res.clone();
        Box::pin(async move { res.commit(final_len).await.map_err(SourceError::Storage) })
    }

    fn fail<'a>(&'a self, msg: String) -> futures::future::BoxFuture<'a, Result<(), Self::Error>> {
        let res = self.res.clone();
        Box::pin(async move { res.fail(msg).await.map_err(SourceError::Storage) })
    }
}

pub struct SessionSource {
    res: AssetResourceType,
    progress: Arc<Progress>,
    len: Option<u64>,
}

impl SessionSource {
    fn new(res: AssetResourceType, progress: Arc<Progress>) -> Self {
        Self {
            res,
            progress,
            len: None,
        }
    }
}

#[async_trait]
impl Source for SessionSource {
    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome> {
        trace!(
            start = range.start,
            end = range.end,
            "kithara-file SessionSource wait_range begin"
        );

        match self
            .res
            .wait_range(range)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?
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

    async fn read_at(&self, offset: u64, len: usize) -> KitharaIoResult<Bytes> {
        trace!(offset, len, "kithara-file SessionSource read_at begin");
        let bytes = self
            .res
            .read_at(offset, len)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?;

        self.progress
            .set_read_pos(offset.saturating_add(bytes.len() as u64));

        trace!(
            offset,
            requested = len,
            got = bytes.len(),
            "kithara-file SessionSource read_at done"
        );
        Ok(bytes)
    }

    fn len(&self) -> Option<u64> {
        self.len
    }
}

pub struct FileSession {
    driver: Arc<FileDriver>,
    net_client: HttpClient,
    command_tx: mpsc::UnboundedSender<FileCommand>,
}

impl FileSession {
    pub fn new(
        asset_id: AssetId,
        url: Url,
        net_client: HttpClient,
        options: FileSourceOptions,
        cache: Option<Arc<AssetStore>>,
    ) -> Self {
        let driver = Arc::new(FileDriver::new(
            asset_id,
            url,
            net_client.clone(),
            options,
            cache,
        ));

        let (command_tx, _command_rx) = mpsc::unbounded_channel::<FileCommand>();

        Self {
            driver,
            net_client,
            command_tx,
        }
    }

    pub fn asset_id(&self) -> AssetId {
        self.driver.asset_id()
    }

    pub async fn stream(
        &self,
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes, FileError>> + Send + '_>> {
        let driver_stream = self.driver.stream().await;
        Box::pin(driver_stream.map(|result| result.map_err(FileError::Driver)))
    }

    pub async fn source(&self) -> Result<SessionSource, FileError> {
        let driver = &self.driver;

        let assets = driver.assets().ok_or_else(|| {
            FileError::Driver(DriverError::Source(SourceError::Assets(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "assets store is required for kithara-file; pass Some(AssetStore)",
                )
                .into(),
            )))
        })?;

        let key = driver.url().into();
        let cancel = CancellationToken::new();
        let res = assets
            .open_streaming_resource(&key, cancel.clone())
            .await
            .map_err(|e| FileError::Driver(DriverError::Source(SourceError::Assets(e))))?;

        let progress = Arc::new(Progress::new());
        Self::spawn_download_writer(
            &self.net_client,
            driver,
            res.clone(),
            progress.clone(),
            cancel,
        );

        Ok(SessionSource::new(res, progress))
    }

    fn spawn_download_writer(
        net_client: &HttpClient,
        driver: &FileDriver,
        res: AssetResourceType,
        progress: Arc<Progress>,
        cancel: CancellationToken,
    ) {
        let _ = progress;

        let net = NetHttp(net_client.clone());
        let sink = AssetSink { res: res.clone() };
        let req = driver.url().clone();
        let cancel_cloned = cancel.clone();

        tokio::spawn(async move {
            let _ = Writer::new(net, req, sink, cancel_cloned)
                .run_with_fail()
                .await;
        });
    }

    pub fn send_command(&self, command: FileCommand) -> Result<(), FileError> {
        self.command_tx
            .send(command)
            .map_err(|_| FileError::DriverStopped)
    }

    pub fn seek_bytes(&self, position: u64) -> Result<(), FileError> {
        self.send_command(FileCommand::SeekBytes(position))
    }
}

impl Clone for FileSession {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            net_client: self.net_client.clone(),
            command_tx: self.command_tx.clone(),
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
    #[error("Core error: {0}")]
    Core(#[from] CoreError),
}

pub type FileResult<T> = Result<T, FileError>;
