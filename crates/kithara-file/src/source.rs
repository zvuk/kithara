#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_assets::{AssetId, AssetStoreBuilder, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_stream::{OpenedSource, StreamError, StreamSource};
use tokio::sync::broadcast;
use url::Url;

use crate::{
    FileResult, FileSession,
    driver::{FileStreamState, SourceError},
    events::FileEvent,
    options::FileParams,
    session::{Progress, SessionSource},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct FileSource;

impl FileSource {
    /// Open a file source from a URL.
    ///
    /// Returns `FileSession` for streaming or random-access reading.
    ///
    /// ## Usage
    ///
    /// ```ignore
    /// use kithara_file::{FileSource, FileParams};
    /// use kithara_assets::StoreOptions;
    ///
    /// let params = FileParams::new(StoreOptions::new("/tmp/cache"));
    /// let session = FileSource::open(url, params).await?;
    /// let source = session.source().await?;
    /// ```
    pub async fn open(url: Url, params: FileParams) -> FileResult<FileSession> {
        let asset_id = AssetId::from_url(&url)?;
        let asset_root = asset_root_for_url(&url);
        let cancel = params.cancel.clone().unwrap_or_default();

        let store = AssetStoreBuilder::new()
            .root_dir(&params.store.cache_dir)
            .asset_root(&asset_root)
            .evict_config(params.store.to_evict_config())
            .cancel(cancel.clone())
            .build();

        let net_client = HttpClient::new(params.net.clone());

        let session = FileSession::new(
            asset_id,
            url,
            net_client,
            Arc::new(store),
            cancel,
            params.event_capacity,
        );

        Ok(session)
    }
}

/// Marker type for file streaming with the unified `Stream<S>` API.
///
/// ## Usage
///
/// ```ignore
/// use kithara_stream::Stream;
/// use kithara_file::{File, FileParams};
/// use kithara_assets::StoreOptions;
///
/// let params = FileParams::new(StoreOptions::new("/tmp/cache"));
/// let stream = Stream::<File>::open(url, params).await?;
/// let events = stream.events();  // Receiver<FileEvent>
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct File;

impl StreamSource for File {
    type Params = FileParams;
    type Event = FileEvent;
    type SourceImpl = SessionSource;

    async fn open(
        url: Url,
        params: Self::Params,
    ) -> Result<OpenedSource<Self::SourceImpl, Self::Event>, StreamError<SourceError>> {
        let asset_root = asset_root_for_url(&url);
        let cancel = params.cancel.clone().unwrap_or_default();

        let store = AssetStoreBuilder::new()
            .root_dir(&params.store.cache_dir)
            .asset_root(&asset_root)
            .evict_config(params.store.to_evict_config())
            .cancel(cancel.clone())
            .build();

        let net_client = HttpClient::new(params.net.clone());

        let state =
            FileStreamState::create(Arc::new(store), net_client.clone(), url, cancel.clone(), params.event_capacity)
                .await
                .map_err(StreamError::Source)?;

        let (events_tx, _) = broadcast::channel(params.event_capacity);
        let progress = Arc::new(Progress::new());

        crate::session::FileSession::spawn_download_writer_static(
            &net_client,
            state.clone(),
            progress.clone(),
        );

        let source = SessionSource::new(
            state.res().clone(),
            progress,
            events_tx.clone(),
            state.len(),
        );

        Ok(OpenedSource {
            source: Arc::new(source),
            events_tx,
        })
    }
}
