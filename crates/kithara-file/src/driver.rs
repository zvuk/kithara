use crate::options::{FileSourceOptions, OptionsError};
use crate::range_policy::RangePolicy;
use async_stream::stream;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_core::AssetId;
use kithara_net::{NetClient, NetError};
use std::pin::Pin;
use thiserror::Error;

#[cfg(feature = "cache")]
use kithara_cache::{AssetCache, CachePath};
#[cfg(feature = "cache")]
use std::sync::Arc;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("Network error: {0}")]
    Net(#[from] NetError),
    #[cfg(feature = "cache")]
    #[error("Cache error: {0}")]
    Cache(#[from] kithara_cache::CacheError),
    #[error("Options error: {0}")]
    Options(#[from] OptionsError),
    #[error("Seek not supported")]
    SeekNotSupported,
}

#[derive(Debug)]
pub enum FileCommand {
    Stop,
    SeekBytes(u64),
}

#[derive(Debug, Clone)]
pub struct FileDriver {
    asset_id: AssetId,
    url: url::Url,
    net_client: NetClient,
    options: FileSourceOptions,
    #[cfg(feature = "cache")]
    cache: Option<Arc<AssetCache>>,
    range_policy: RangePolicy,
}

impl FileDriver {
    pub fn new(
        asset_id: AssetId,
        url: url::Url,
        net_client: NetClient,
        options: FileSourceOptions,
        #[cfg(feature = "cache")] cache: Option<Arc<AssetCache>>,
    ) -> Self {
        let range_policy = RangePolicy::new(options.enable_range_seek);
        Self {
            asset_id,
            url,
            net_client,
            options: options.clone(),
            #[cfg(feature = "cache")]
            cache,
            range_policy,
        }
    }

    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub async fn stream(
        &self,
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes, DriverError>> + Send + '_>> {
        let client = self.net_client.clone();
        let url = self.url.clone();

        #[cfg(feature = "cache")]
        let cache = self.cache.clone();

        let _range_policy = &self.range_policy;

        Box::pin(stream! {
            // Check cache first if available
            #[cfg(feature = "cache")]
            if let Some(ref cache) = cache {
                let asset_handle = cache.asset(self.asset_id);
                let body_path = match CachePath::new(vec!["file".to_string(), "body".to_string()]) {
                    Ok(path) => path,
                    Err(e) => {
                        yield Err(DriverError::Cache(e));
                        return;
                    }
                };

                if let Some(mut file) = asset_handle.open(&body_path).unwrap_or(None) {
                    use std::io::Read;
                    let mut buffer = vec![0u8; 8192];
                    loop {
                        match file.read(&mut buffer) {
                            Ok(0) => return, // EOF
                            Ok(n) => {
                                yield Ok(Bytes::copy_from_slice(&buffer[..n]));
                            }
                            Err(e) => {
                                yield Err(DriverError::Cache(kithara_cache::CacheError::Io(e)));
                                return;
                            }
                        }
                    }
                }
            }

            // Stream from network
            let mut stream = match client.stream(url, None).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(DriverError::Net(e));
                    return;
                }
            };

            #[cfg(feature = "cache")]
            let mut cached_bytes = Vec::new();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        #[cfg(feature = "cache")]
                        if let Some(ref _cache) = cache {
                            cached_bytes.extend_from_slice(&bytes);
                        }

                        yield Ok(bytes);
                    }
                    Err(e) => {
                        yield Err(DriverError::Net(e));
                        break;
                    }
                }
            }

            // Write to cache after successful download
            #[cfg(feature = "cache")]
            if let Some(ref cache) = cache && !cached_bytes.is_empty() {
                let asset_handle = cache.asset(self.asset_id);
                let body_path = match CachePath::new(vec!["file".to_string(), "body".to_string()]) {
                    Ok(path) => path,
                    Err(_e) => {
                        // Cache write failure shouldn't affect stream result
                        return;
                    }
                };

                let _ = asset_handle.put_atomic(&body_path, &cached_bytes);
            }
        })
    }

    pub async fn seek_to(&mut self, position: u64) -> Result<(), DriverError> {
        if !self.options.enable_range_seek {
            return Err(DriverError::SeekNotSupported);
        }

        self.range_policy.update_position(position)?;
        // TODO: Implement actual range request seeking logic
        // This would restart the stream from new position using HTTP Range headers

        Ok(())
    }

    pub async fn handle_command(&mut self, command: FileCommand) -> Result<(), DriverError> {
        match command {
            FileCommand::Stop => {
                // Stop driver loop - this would be handled by stream implementation
                Ok(())
            }
            FileCommand::SeekBytes(position) => self.seek_to(position).await,
        }
    }
}