use crate::driver::{DriverError, FileCommand, FileDriver};
use crate::options::FileSourceOptions;
use async_stream::stream;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_core::AssetId;
use kithara_net::NetClient;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

#[cfg(feature = "cache")]
use kithara_cache::AssetCache;

#[derive(Debug)]
pub struct FileSession {
    driver: Arc<FileDriver>,
    command_tx: mpsc::UnboundedSender<FileCommand>,
}

impl FileSession {
    pub fn new(
        asset_id: AssetId,
        url: url::Url,
        net_client: NetClient,
        options: FileSourceOptions,
        #[cfg(feature = "cache")] cache: Option<Arc<AssetCache>>,
    ) -> Self {
        let driver = Arc::new(FileDriver::new(
            asset_id,
            url,
            net_client,
            options,
            #[cfg(feature = "cache")]
            cache,
        ));

        let (command_tx, _command_rx) = mpsc::unbounded_channel::<FileCommand>();
        // Note: In a full implementation, we'd have a command receiver
        // that the driver loop would monitor. For now, this is the placeholder.

        Self { driver, command_tx }
    }

    pub fn asset_id(&self) -> AssetId {
        self.driver.asset_id()
    }

    pub fn stream(&self) -> Pin<Box<dyn Stream<Item = Result<Bytes, FileError>> + Send + '_>> {
        Box::pin(stream! {
            let mut driver_stream = self.driver.stream().await;
            while let Some(result) = driver_stream.next().await {
                yield result.map_err(FileError::Driver);
            }
        })
    }

    /// Send a command to the driver
    ///
    /// Returns an error if the command channel is closed
    pub fn send_command(&self, command: FileCommand) -> Result<(), FileError> {
        self.command_tx
            .send(command)
            .map_err(|_| FileError::DriverStopped)
    }

    /// Convenience method for stopping the driver
    pub fn stop(&self) -> Result<(), FileError> {
        self.send_command(FileCommand::Stop)
    }

    /// Convenience method for seeking to a byte position
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
}

pub type FileResult<T> = Result<T, FileError>;
