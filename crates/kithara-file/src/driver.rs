use crate::options::{FileSourceOptions, OptionsError};
use crate::range_policy::RangePolicy;
use bytes::Bytes;
use futures::{Stream as FuturesStream, StreamExt};
use kithara_assets::AssetCache;
use kithara_core::AssetId;
use kithara_net::{HttpClient, NetError};
use kithara_stream::{Message, Source, SourceStream, Stream, StreamError, StreamParams};
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;

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
    Cache(#[from] kithara_assets::CacheError),

    #[error("Offline miss: content not in cache")]
    OfflineMiss,
}

#[derive(Debug)]
pub enum FileCommand {
    /// Command to seek to a specific byte position.
    ///
    /// The position is absolute (from start of resource).
    /// See `FileSession::seek_bytes` for detailed contract.
    SeekBytes(u64),
}

#[derive(Debug, Clone)]
pub struct FileDriver {
    asset_id: AssetId,
    url: url::Url,
    net_client: HttpClient,
    #[allow(dead_code)]
    options: FileSourceOptions,
    cache: Option<Arc<AssetCache>>,
    range_policy: RangePolicy,
}

impl FileDriver {
    pub fn new(
        asset_id: AssetId,
        url: url::Url,
        net_client: HttpClient,
        options: FileSourceOptions,
        cache: Option<Arc<AssetCache>>,
    ) -> Self {
        let range_policy = RangePolicy::new(options.enable_range_seek);
        Self {
            asset_id,
            url,
            net_client,
            options: options.clone(),
            cache,
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
            options: self.options.clone(),
            cache: self.cache.clone(),
        };
        let params = StreamParams {
            offline_mode: self.options.offline_mode,
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

#[derive(Debug, Clone)]
struct FileStream {
    asset_id: AssetId,
    url: url::Url,
    net_client: HttpClient,
    options: FileSourceOptions,
    cache: Option<Arc<AssetCache>>,
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
        let cache = self.cache.clone();
        let asset_id = self.asset_id;

        let offline_mode = params.offline_mode;

        Ok(Box::pin(async_stream::stream! {
            // Cache/Assets integration is being redesigned (resource-based API).
            // Disable the old cache-first path until the new Assets contract is wired in.
            let _ = &cache;
            let _ = asset_id;

            if offline_mode {
                yield Err(StreamError::Source(SourceError::OfflineMiss));
                return;
            }

            // Network stream.
            let mut stream = match client.stream(url, None).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(StreamError::Source(SourceError::Net(e)));
                    return;
                }
            };

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        yield Ok(Message::Data(bytes));
                    }
                    Err(e) => {
                        yield Err(StreamError::Source(SourceError::Net(e)));
                        return;
                    }
                }
            }

            // Assets integration is being redesigned (streaming+atomic resources).
            // Old "cache whole body at end" behavior is intentionally disabled.
            let _ = &cache;
        }))
    }

    fn seek_bytes(&mut self, _pos: u64) -> Result<(), StreamError<Self::Error>> {
        Err(StreamError::SeekNotSupported)
    }

    fn supports_seek(&self) -> bool {
        false
    }
}
