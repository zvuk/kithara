//! File inner stream implementation.
//!
//! Provides `FileInner` - a sync `Read + Seek` adapter for file streams.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_stream::{StreamMsg, StreamType, SyncReader, SyncReaderParams, Writer};
use tokio::sync::broadcast;
use url::Url;

use crate::{
    error::SourceError,
    events::FileEvent,
    options::FileParams,
    session::{FileStreamState, Progress, SessionSource},
};

/// Configuration for file stream.
#[derive(Clone)]
pub struct FileConfig {
    /// File URL.
    pub url: Url,
    /// File parameters.
    pub params: FileParams,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost/audio.mp3").expect("valid default URL"),
            params: FileParams::default(),
        }
    }
}

impl FileConfig {
    /// Create config with URL.
    pub fn new(url: Url) -> Self {
        Self {
            url,
            params: FileParams::default(),
        }
    }

    /// Set file parameters.
    pub fn with_params(mut self, params: FileParams) -> Self {
        self.params = params;
        self
    }
}

/// File inner stream implementing `Read + Seek`.
///
/// This wraps `SyncReader<SessionSource>` to provide sync access to file streams.
pub struct FileInner {
    reader: SyncReader<SessionSource>,
}

impl FileInner {
    /// Create new file inner stream.
    pub async fn new(config: FileConfig) -> Result<Self, SourceError> {
        let url = config.url;
        let params = config.params;

        let asset_root = asset_root_for_url(&url);
        let cancel = params.cancel.clone().unwrap_or_default();

        let store = AssetStoreBuilder::new()
            .root_dir(&params.store.cache_dir)
            .asset_root(&asset_root)
            .evict_config(params.store.to_evict_config())
            .cancel(cancel.clone())
            .build();

        let net_client = HttpClient::new(params.net.clone());

        let state = FileStreamState::create(
            Arc::new(store),
            net_client.clone(),
            url,
            cancel.clone(),
            params.events_tx.clone(),
        )
        .await?;

        let progress = Arc::new(Progress::new());

        spawn_download_writer(
            &net_client,
            state.clone(),
            progress.clone(),
            state.events().clone(),
        );

        let source = SessionSource::new(
            state.res().clone(),
            progress,
            state.events().clone(),
            state.len(),
        );

        let reader = SyncReader::new(Arc::new(source), SyncReaderParams::default());

        Ok(Self { reader })
    }

    /// Get current read position.
    pub fn position(&self) -> u64 {
        self.reader.position()
    }
}

impl Read for FileInner {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Seek for FileInner {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.reader.seek(pos)
    }
}

/// Marker type for file streaming.
pub struct File;

impl StreamType for File {
    type Config = FileConfig;
    type Inner = FileInner;
    type Error = SourceError;

    async fn create(config: Self::Config) -> Result<Self::Inner, Self::Error> {
        FileInner::new(config).await
    }
}

fn spawn_download_writer(
    net_client: &HttpClient,
    state: Arc<FileStreamState>,
    progress: Arc<Progress>,
    events_tx: broadcast::Sender<FileEvent>,
) {
    let net = net_client.clone();
    let url = state.url().clone();
    let len = state.len();
    let res = state.res().clone();
    let cancel = state.cancel().clone();

    tokio::spawn(async move {
        let writer = Writer::<_, _, FileEvent>::new(net, url, None, res, cancel).with_event(
            move |offset, _len| {
                progress.set_download_pos(offset);
                let percent =
                    len.map(|len| ((offset as f64 / len as f64) * 100.0).min(100.0) as f32);
                FileEvent::DownloadProgress { offset, percent }
            },
            move |msg| {
                if let StreamMsg::Event(ev) = msg {
                    let _ = events_tx.send(ev);
                }
            },
        );

        let _ = writer.run_with_fail().await;
    });
}
