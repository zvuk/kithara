use crate::driver::{DriverError, FileCommand, FileDriver};
use crate::options::FileSourceOptions;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_cache::AssetCache;
use kithara_core::{AssetId, CoreError};
use kithara_net::HttpClient;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug)]
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
        cache: Option<Arc<AssetCache>>,
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

    /// Send a command to the driver
    ///
    /// Returns an error if the command channel is closed
    pub fn send_command(&self, command: FileCommand) -> Result<(), FileError> {
        self.command_tx
            .send(command)
            .map_err(|_| FileError::DriverStopped)
    }

    /// Seek to a byte position in the stream.
    ///
    /// # Contract
    ///
    /// - Seeking is best-effort and may not be supported (`SeekNotSupported` error).
    /// - When `enable_range_seek` is `true` in options, range requests may be used.
    /// - Seeking beyond available content (EOF) should return `InvalidSeekPosition` error
    ///   if the total size is known, otherwise may succeed but result in immediate EOF.
    /// - After a successful seek, subsequent reads start from the new position.
    /// - Seek position 0 resets to the beginning of the resource.
    /// - The stream must be active (consumer reading) for seek commands to be processed.
    ///
    /// # Errors
    ///
    /// - `SeekNotSupported`: when `enable_range_seek` is `false` or seek not implemented.
    /// - `InvalidSeekPosition`: when position is beyond known content size.
    /// - `DriverStopped`: when driver is not running or command channel closed.
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
