use crate::options::{FileSourceOptions, OptionsError};
use crate::range_policy::RangePolicy;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_cache::{AssetCache, CachePath};
use kithara_core::AssetId;
use kithara_net::{NetClient, NetError};
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("Network error: {0}")]
    Net(#[from] NetError),
    #[error("Cache error: {0}")]
    Cache(#[from] kithara_cache::CacheError),
    #[error("Options error: {0}")]
    Options(#[from] OptionsError),
    #[error("Seek not supported")]
    SeekNotSupported,
    #[error("Offline miss: content not in cache")]
    OfflineMiss,
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
    #[allow(dead_code)]
    options: FileSourceOptions,
    cache: Option<Arc<AssetCache>>,
    range_policy: RangePolicy,
}

impl FileDriver {
    pub fn new(
        asset_id: AssetId,
        url: url::Url,
        net_client: NetClient,
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
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes, DriverError>> + Send + '_>> {
        let client = self.net_client.clone();
        let url = self.url.clone();
        let cache = self.cache.clone();
        let asset_id = self.asset_id;
        let offline_mode = self.options.offline_mode;

        // Create a bounded channel for backpressure (cancellation via drop)
        let (tx, rx) = mpsc::channel(16); // Buffer 16 chunks

        // Spawn driver task
        let _driver_task = tokio::spawn(async move {
            // Check cache first if available
            #[allow(unused_assignments)]
            let mut cache_hit = false;
            if let Some(ref cache) = cache {
                let asset_handle = cache.asset(asset_id);
                let body_path = match CachePath::new(vec!["file".to_string(), "body".to_string()]) {
                    Ok(path) => path,
                    Err(e) => {
                        let _ = tx.send(Err(DriverError::Cache(e))).await;
                        return;
                    }
                };

                if let Some(mut file) = asset_handle.open(&body_path).unwrap_or(None) {
                    // Cache hit - stream from cache
                    cache_hit = true;
                    use std::io::Read;
                    let mut buffer = vec![0u8; 8192];
                    loop {
                        match file.read(&mut buffer) {
                            Ok(0) => return, // EOF
                            Ok(n) => {
                                let chunk = Bytes::copy_from_slice(&buffer[..n]);
                                if tx.send(Ok(chunk)).await.is_err() {
                                    // Receiver dropped, cancel download
                                    return;
                                }
                            }
                            Err(e) => {
                                let _ = tx
                                    .send(Err(DriverError::Cache(kithara_cache::CacheError::Io(e))))
                                    .await;
                                return;
                            }
                        }
                    }
                }
            }

            // If offline mode and cache miss, return OfflineMiss error
            if offline_mode && !cache_hit {
                let _ = tx.send(Err(DriverError::OfflineMiss)).await;
                return;
            }

            // Stream from network
            let mut stream = match client.stream(url, None).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Err(DriverError::Net(e))).await;
                    return;
                }
            };

            let mut cached_bytes = Vec::new();

            while let Some(chunk_result) = stream.next().await {
                // Check if receiver is still alive
                if tx.is_closed() {
                    // Consumer dropped the stream, cancel download
                    return;
                }

                match chunk_result {
                    Ok(bytes) => {
                        if let Some(ref _cache) = cache {
                            cached_bytes.extend_from_slice(&bytes);
                        }

                        if tx.send(Ok(bytes)).await.is_err() {
                            // Receiver dropped, cancel download
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(DriverError::Net(e))).await;
                        break;
                    }
                }
            }

            // Write to cache after successful download
            if let Some(ref cache) = cache
                && !cached_bytes.is_empty()
            {
                let asset_handle = cache.asset(asset_id);
                let body_path = match CachePath::new(vec!["file".to_string(), "body".to_string()]) {
                    Ok(path) => path,
                    Err(_e) => {
                        // Cache write failure shouldn't affect stream result
                        return;
                    }
                };

                // Ignore cache write errors - they shouldn't fail the stream
                let _ = asset_handle.put_atomic(&body_path, &cached_bytes);
            }
        });

        // Return stream that reads from channel
        // When this stream is dropped, the channel receiver will be dropped,
        // which will cause the driver task to detect tx.is_closed() and cancel
        Box::pin(ReceiverStream::new(rx))
    }

    #[allow(dead_code)]
    pub async fn seek_to(&mut self, position: u64) -> Result<(), DriverError> {
        if !self.options.enable_range_seek {
            return Err(DriverError::SeekNotSupported);
        }

        self.range_policy.update_position(position)?;
        // TODO: Implement actual range request seeking logic
        // This would restart the stream from new position using HTTP Range headers

        Ok(())
    }

    #[allow(dead_code)]
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
