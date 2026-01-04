use std::{ops::Range, pin::Pin, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_assets::{AssetStore, ResourceKey};
use kithara_core::{AssetId, CoreError};
use kithara_io::{IoError as KitharaIoError, IoResult as KitharaIoResult, Source, WaitOutcome};
use kithara_net::HttpClient;
use kithara_storage::{Resource, StreamingResourceExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::{
    driver::{DriverError, FileCommand, FileDriver, SourceError},
    options::FileSourceOptions,
};

pub struct SessionSource {
    res: kithara_assets::AssetResource<
        kithara_storage::StreamingResource,
        kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
    >,
    cancel: CancellationToken,
    // Length is generally unknown for progressive HTTP unless the writer commits with known len.
    // We don't currently plumb a "query length" API through assets/storage, so keep it None.
    len: Option<u64>,
}

impl SessionSource {
    fn new(
        res: kithara_assets::AssetResource<
            kithara_storage::StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            res,
            cancel,
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
    command_tx: mpsc::UnboundedSender<FileCommand>,
}

impl FileSession {
    pub fn new(
        asset_id: AssetId,
        url: url::Url,
        net_client: HttpClient,
        options: FileSourceOptions,
        cache: Option<Arc<AssetStore>>,
    ) -> Self {
        let driver = Arc::new(FileDriver::new(asset_id, url, net_client, options, cache));

        let (command_tx, _command_rx) = mpsc::unbounded_channel::<FileCommand>();
        // Note: In a full implementation, we'd have a command receiver
        // that the driver loop would monitor. For now, this is the placeholder.

        Self { driver, command_tx }
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

    /// Create an I/O `Source` adapter for this session.
    ///
    /// This opens exactly one streaming resource and starts exactly one writer task that fills it.
    /// The returned [`SessionSource`] can be wrapped by `kithara-io::Reader` to satisfy
    /// sync consumers like `rodio::Decoder` (`Read + Seek`).
    pub async fn source(&self) -> Result<SessionSource, FileError> {
        let driver = &self.driver;

        let Some(assets) = driver.assets() else {
            return Err(FileError::Driver(DriverError::Source(SourceError::Assets(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "assets store is required for kithara-file; pass Some(AssetStore)",
                )
                .into(),
            ))));
        };

        // MP3/progressive file layout (contracted in kithara-assets README examples).
        let asset_root = hex::encode(driver.asset_id().as_bytes());
        let key = ResourceKey::new(asset_root, "media/audio.mp3");

        let cancel = CancellationToken::new();
        let res = assets
            .open_streaming_resource(&key, cancel.clone())
            .await
            .map_err(|e| FileError::Driver(DriverError::Source(SourceError::Assets(e))))?;

        // Start download writer ONCE for this SessionSource.
        let url = driver.url().clone();
        let client = driver.net_client().clone();
        tokio::spawn({
            let res = res.clone();
            async move {
                trace!(%url, "kithara-file SessionSource writer task: starting download");

                let mut stream = match client.stream(url, None).await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("kithara-file SessionSource writer task: net error: {e}");
                        let _ = res.fail(format!("net error: {e}")).await;
                        return;
                    }
                };

                let mut off: u64 = 0;
                while let Some(chunk_result) = stream.next().await {
                    match chunk_result {
                        Ok(bytes) => {
                            if let Err(e) = res.write_at(off, &bytes).await {
                                warn!(
                                    off,
                                    bytes = bytes.len(),
                                    "kithara-file SessionSource writer task: write_at error: {e}"
                                );
                                let _ = res.fail(format!("storage write_at error: {e}")).await;
                                return;
                            }
                            off = off.saturating_add(bytes.len() as u64);
                        }
                        Err(e) => {
                            warn!("kithara-file SessionSource writer task: net stream error: {e}");
                            let _ = res.fail(format!("net stream error: {e}")).await;
                            return;
                        }
                    }
                }

                debug!(
                    final_len = off,
                    "kithara-file SessionSource writer task: commit"
                );
                let _ = res.commit(Some(off)).await;
            }
        });

        Ok(SessionSource::new(res, cancel))
    }

    /// Send a command to the driver
    ///
    /// Returns an error if the command channel is closed
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
    Net(#[from] kithara_net::NetError),
    #[error("Core error: {0}")]
    Core(#[from] CoreError),
}

pub type FileResult<T> = Result<T, FileError>;
