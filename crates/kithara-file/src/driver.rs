use std::{pin::Pin, sync::Arc};

use bytes::Bytes;
use futures::{Stream as FuturesStream, StreamExt, pin_mut};
use kithara_assets::{
    AssetId, AssetResource, AssetStore, Assets, CachedAssets, DiskAssetStore, EvictAssets,
    LeaseGuard,
};
use kithara_net::{HttpClient, Net, NetError};
use kithara_storage::{ResourceStatus, StorageError, StreamingResource};
use kithara_stream::{
    Engine, EngineHandle, EngineParams, EngineSource, Reader, ReaderError, StreamError, StreamMsg,
    Writer, WriterTask,
};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::events::FileEvent;

// Type aliases for complex types
type AssetResourceType =
    AssetResource<StreamingResource, LeaseGuard<CachedAssets<EvictAssets<DiskAssetStore>>>>;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("Source error: {0}")]
    Source(#[from] SourceError),

    #[error("Stream error: {0}")]
    Stream(#[from] StreamError<SourceError>),

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
    Storage(#[from] StorageError),

    #[error("Writer error: {0}")]
    Writer(String),

    #[error("Reader error: {0}")]
    Reader(String),
}

#[derive(Clone)]
pub struct FileDriver {
    asset_id: AssetId,
    url: Url,
    net_client: HttpClient,
    assets: Arc<AssetStore>,
    cancel: CancellationToken,
    event_capacity: usize,
}

impl FileDriver {
    pub fn new(
        asset_id: AssetId,
        url: Url,
        net_client: HttpClient,
        assets: Arc<AssetStore>,
        cancel: CancellationToken,
        event_capacity: usize,
    ) -> Self {
        Self {
            asset_id,
            url,
            net_client,
            assets,
            cancel,
            event_capacity,
        }
    }

    pub fn assets(&self) -> Arc<AssetStore> {
        Arc::clone(&self.assets)
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    pub fn event_capacity(&self) -> usize {
        self.event_capacity
    }

    pub async fn stream_with_handle(
        &self,
    ) -> (
        EngineHandle,
        Pin<Box<dyn FuturesStream<Item = Result<Bytes, DriverError>> + Send + '_>>,
    ) {
        let source = FileStream {
            url: self.url.clone(),
            net_client: self.net_client.clone(),
            assets: Arc::clone(&self.assets),
            cancel: self.cancel.clone(),
            event_capacity: self.event_capacity,
            pos: 0,
        };

        let params = EngineParams::default();
        let engine = Engine::new(source, params);
        let handle = engine.handle();
        let stream = engine.into_stream().filter_map(|item| async move {
            match item {
                Ok(StreamMsg::Data(b)) => Some(Ok(b)),
                Ok(StreamMsg::Control(_)) | Ok(StreamMsg::Event(_)) => None,
                Err(e) => Some(Err(DriverError::from(e))),
            }
        });

        (handle, Box::pin(stream))
    }
}

#[derive(Clone)]
struct FileStream {
    url: Url,
    net_client: HttpClient,
    assets: Arc<AssetStore>,
    cancel: CancellationToken,
    event_capacity: usize,
    pos: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct FileStreamState {
    pub(crate) url: Url,
    pub(crate) cancel: CancellationToken,
    pub(crate) res: AssetResourceType,
    pub(crate) events: broadcast::Sender<FileEvent>,
    pub(crate) len: Option<u64>,
}

impl FileStreamState {
    pub(crate) async fn create(
        assets: Arc<AssetStore>,
        net_client: HttpClient,
        url: Url,
        cancel: CancellationToken,
        event_capacity: usize,
    ) -> Result<Arc<Self>, SourceError> {
        let headers = net_client.head(url.clone(), None).await?;
        let len = headers
            .get("content-length")
            .or_else(|| headers.get("Content-Length"))
            .and_then(|v| v.parse::<u64>().ok());

        let key = (&url).into();
        let res = assets
            .open_streaming_resource(&key)
            .await
            .map_err(SourceError::Assets)?;

        let (events, _) = broadcast::channel(event_capacity);

        Ok(Arc::new(FileStreamState {
            url,
            cancel,
            res,
            events,
            len,
        }))
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn res(&self) -> &AssetResourceType {
        &self.res
    }

    pub(crate) fn events(&self) -> &broadcast::Sender<FileEvent> {
        &self.events
    }

    pub(crate) fn len(&self) -> Option<u64> {
        self.len
    }

    pub(crate) fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }
}

impl EngineSource for FileStream {
    type Error = SourceError;
    type Control = ();
    type Event = FileEvent;
    type State = Arc<FileStreamState>;

    fn init(
        &mut self,
        _params: EngineParams,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<Self::State, StreamError<Self::Error>>>
                + Send
                + 'static,
        >,
    > {
        let assets = Arc::clone(&self.assets);
        let url = self.url.clone();
        let net_client = self.net_client.clone();
        let cancel = self.cancel.clone();
        let event_capacity = self.event_capacity;

        Box::pin(async move {
            FileStreamState::create(assets, net_client, url, cancel, event_capacity)
                .await
                .map_err(StreamError::Source)
        })
    }

    fn open_reader(
        &mut self,
        state: &Self::State,
        _params: EngineParams,
    ) -> Result<
        Pin<
            Box<
                dyn FuturesStream<Item = Result<StreamMsg<(), FileEvent>, StreamError<SourceError>>>
                    + Send
                    + 'static,
            >,
        >,
        StreamError<SourceError>,
    > {
        let start_pos = self.pos;
        let state = state.clone();

        Ok(Box::pin(async_stream::stream! {
            let reader_stream = Reader::new(state.res.clone(), start_pos, 64 * 1024).into_stream::<FileEvent>();
            pin_mut!(reader_stream);

            let mut events_rx = state.events.subscribe();
            let mut events_closed = false;
            let mut pos = start_pos;

            loop {
                tokio::select! {
                    maybe_ev = events_rx.recv(), if !events_closed => {
                        match maybe_ev {
                            Ok(ev) => {
                                yield Ok(StreamMsg::Event(ev));
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                continue;
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                events_closed = true;
                                continue;
                            }
                        }
                    }
                    item = reader_stream.next() => {
                        let Some(item) = item else {
                            return;
                        };
                        match item {
                            Ok(StreamMsg::Data(bytes)) => {
                                pos = pos.saturating_add(bytes.len() as u64);
                                let percent = state.len.map(|len| {
                                    ((pos as f64 / len as f64) * 100.0).min(100.0) as f32
                                });
                                let _ = state.events.send(FileEvent::PlaybackProgress {
                                    position: pos,
                                    percent,
                                });
                                yield Ok(StreamMsg::Data(bytes));
                            }
                            Ok(StreamMsg::Control(_)) | Ok(StreamMsg::Event(_)) => {}
                            Err(StreamError::Source(ReaderError::Wait(storage_err)))
                            | Err(StreamError::Source(ReaderError::Read(storage_err))) => {
                                yield Err(StreamError::Source(SourceError::Storage(storage_err)));
                                return;
                            }
                            Err(StreamError::Source(other)) => {
                                yield Err(StreamError::Source(SourceError::Reader(other.to_string())));
                                return;
                            }
                            Err(StreamError::WriterJoin(e)) => {
                                yield Err(StreamError::Source(SourceError::Reader(e)));
                                return;
                            }
                            Err(StreamError::SeekNotSupported) | Err(StreamError::ChannelClosed) => {
                                yield Err(StreamError::Source(SourceError::Reader("reader error".to_string())));
                                return;
                            }
                            Err(StreamError::InvalidSeek) | Err(StreamError::UnknownLength) => {
                                yield Err(StreamError::Source(SourceError::Reader("invalid seek or unknown length".to_string())));
                                return;
                            }
                        }
                    }
                }
            }
        }))
    }

    fn start_writer(
        &mut self,
        state: &Self::State,
        _params: EngineParams,
    ) -> Result<WriterTask<SourceError>, StreamError<SourceError>> {
        let net = self.net_client.clone();
        let url = self.url.clone();
        let res = state.res.clone();
        let cancel = state.cancel.clone();
        let events = state.events.clone();
        let len = state.len;

        Ok(tokio::spawn(async move {
            if matches!(res.inner().status().await, ResourceStatus::Committed { .. }) {
                return Ok(());
            }

            let writer = Writer::<_, _, FileEvent>::new(net, url, None, res, cancel).with_event(
                move |offset, _len| {
                    let percent =
                        len.map(|len| ((offset as f64 / len as f64) * 100.0).min(100.0) as f32);
                    FileEvent::DownloadProgress { offset, percent }
                },
                move |msg| {
                    if let StreamMsg::Event(ev) = msg {
                        let _ = events.send(ev);
                    }
                },
            );

            let _ = writer.run_with_fail().await;
            Ok(())
        }))
    }

    fn seek_bytes(&mut self, pos: u64) -> Result<(), StreamError<SourceError>> {
        self.pos = pos;
        Ok(())
    }

    fn supports_seek(&self) -> bool {
        true
    }
}
