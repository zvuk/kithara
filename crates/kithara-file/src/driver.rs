use std::{pin::Pin, sync::Arc};

use bytes::Bytes;
use futures::{Stream as FuturesStream, StreamExt};
use kithara_assets::{AssetStore, ResourceKey};
use kithara_core::AssetId;
use kithara_net::{HttpClient, NetError};
use kithara_stream::{Message, Source, SourceStream, Stream, StreamError, StreamParams};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    internal::{Feeder, Fetcher},
    options::{FileSourceOptions, OptionsError},
    range_policy::RangePolicy,
};

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("Source error: {0}")]
    Source(#[from] SourceError),

    #[error("Stream error: {0}")]
    Stream(#[from] kithara_stream::StreamError<SourceError>),

    #[error("Options error: {0}")]
    Options(#[from] OptionsError),

    #[error("Seek not supported")]
    SeekNotSupported,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("Network error: {0}")]
    Net(#[from] NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),

    #[error("Storage error: {0}")]
    Storage(#[from] kithara_storage::StorageError),
}

#[derive(Debug)]
pub enum FileCommand {
    /// Command to seek to a specific byte position.
    ///
    /// The position is absolute (from start of resource).
    /// See `FileSession::seek_bytes` for detailed contract.
    SeekBytes(u64),
}

#[derive(Clone)]
pub struct FileDriver {
    asset_id: AssetId,
    url: url::Url,
    net_client: HttpClient,
    #[allow(dead_code)]
    options: FileSourceOptions,
    assets: Option<Arc<AssetStore>>,
    range_policy: RangePolicy,
}

impl FileDriver {
    pub fn assets(&self) -> Option<Arc<AssetStore>> {
        self.assets.clone()
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn net_client(&self) -> &HttpClient {
        &self.net_client
    }
}

impl FileDriver {
    pub fn new(
        asset_id: AssetId,
        url: url::Url,
        net_client: HttpClient,
        options: FileSourceOptions,
        assets: Option<Arc<AssetStore>>,
    ) -> Self {
        let range_policy = RangePolicy::new(options.enable_range_seek);
        Self {
            asset_id,
            url,
            net_client,
            options: options.clone(),
            assets,
            range_policy,
        }
    }

    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub async fn stream(
        &self,
    ) -> Pin<Box<dyn FuturesStream<Item = Result<Bytes, DriverError>> + Send + '_>> {
        // We intentionally rely on `kithara-stream` for orchestration and command handling.
        // Stopping is done by dropping the returned stream.
        let source = FileStream {
            asset_id: self.asset_id,
            url: self.url.clone(),
            net_client: self.net_client.clone(),
            assets: self.assets.clone(),
            pos: 0,
        };
        let params = StreamParams {
            offline_mode: false,
        };
        let stream = Stream::new(source, params).into_byte_stream();
        Box::pin(stream.map(|r| r.map_err(DriverError::from)))
    }

    #[allow(dead_code)]
    /// Seek to a byte position.
    ///
    /// # Contract
    ///
    /// - Requires `enable_range_seek` to be `true` in options.
    /// - Validates position against known content size (if known).
    /// - Updates internal range policy state.
    /// - Actual range request implementation is TODO.
    ///
    /// # Errors
    ///
    /// - `SeekNotSupported`: when `enable_range_seek` is `false`.
    /// - `InvalidSeekPosition`: when position is beyond known content size.
    pub async fn seek_to(&mut self, position: u64) -> Result<(), DriverError> {
        if !self.options.enable_range_seek {
            return Err(DriverError::SeekNotSupported);
        }

        self.range_policy.update_position(position)?;
        // TODO: implement range seeking via kithara-stream command path once the file source
        // supports reopen-from-position.
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn handle_command(&mut self, command: FileCommand) -> Result<(), DriverError> {
        match command {
            FileCommand::SeekBytes(position) => self.seek_to(position).await,
        }
    }
}

// Fetcher/Feeder live in `crate::internal`.
// Keeping these loops in one place avoids duplication between the `kithara-stream` path
// and the `kithara-io` (rodio) path.

#[derive(Clone)]
struct FileStream {
    asset_id: AssetId,
    url: url::Url,
    net_client: HttpClient,
    assets: Option<Arc<AssetStore>>,
    pos: u64,
}

impl Source for FileStream {
    type Error = SourceError;
    type Control = ();

    fn open(
        &mut self,
        params: StreamParams,
    ) -> Result<SourceStream<Self::Control, Self::Error>, StreamError<Self::Error>> {
        let client = self.net_client.clone();
        let url = self.url.clone();
        let assets = self.assets.clone();
        let asset_root = hex::encode(self.asset_id.as_bytes());
        let start_pos = self.pos;
        let _offline_mode = params.offline_mode;

        Ok(Box::pin(async_stream::stream! {
            let Some(assets) = assets else {
                yield Err(StreamError::Source(SourceError::Assets(
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "assets store is required for kithara-file streaming; pass Some(AssetStore) to FileSource::open",
                    )
                    .into(),
                )));
                return;
            };

            // MP3/progressive file layout (contracted in kithara-assets README examples).
            let key = ResourceKey::new(asset_root, "media/audio.mp3");
            let cancel = CancellationToken::new();

            let res = match assets.open_streaming_resource(&key, cancel).await {
                Ok(r) => r,
                Err(e) => {
                    yield Err(StreamError::Source(SourceError::Assets(e)));
                    return;
                }
            };

            Fetcher::new(client, url).spawn(res.clone());

            let feeder = Feeder::new(start_pos, 64 * 1024);

            let mut s = std::pin::pin!(feeder.stream(res));
            while let Some(item) = s.next().await {
                match item {
                    Ok(bytes) => yield Ok(Message::Data(bytes)),
                    Err(e) => {
                        yield Err(StreamError::Source(SourceError::Storage(e)));
                        return;
                    }
                }
            }
        }))
    }

    fn seek_bytes(&mut self, pos: u64) -> Result<(), StreamError<Self::Error>> {
        self.pos = pos;
        Ok(())
    }

    fn supports_seek(&self) -> bool {
        true
    }
}
