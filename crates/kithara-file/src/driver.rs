use std::{ops::Range, pin::Pin, sync::Arc};

use bytes::Bytes;
use futures::{Stream as FuturesStream, StreamExt, pin_mut, stream::BoxStream};
use kithara_assets::{
    AssetResource, AssetStore, AssetsError, DiskAssetStore, EvictAssets, LeaseGuard,
};
use kithara_core::AssetId;
use kithara_net::{HttpClient, NetError};
use kithara_storage::{
    Resource, StorageError, StreamingResource, StreamingResourceExt,
    WaitOutcome as StorageWaitOutcome,
};
use kithara_stream::{
    Engine, EngineSource, Net, ReadSource, Reader, ReaderError, StreamError, StreamMsg,
    StreamParams, WaitOutcome, WriteSink, Writer, WriterTask,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    options::{FileSourceOptions, OptionsError},
    range_policy::RangePolicy,
};

// Type aliases for complex types
type AssetResourceType = AssetResource<StreamingResource, LeaseGuard<EvictAssets<DiskAssetStore>>>;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("Source error: {0}")]
    Source(#[from] SourceError),

    #[error("Stream error: {0}")]
    Stream(#[from] StreamError<SourceError>),

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
    Assets(#[from] AssetsError),

    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("Writer error: {0}")]
    Writer(String),

    #[error("Reader error: {0}")]
    Reader(String),
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
    url: Url,
    net_client: HttpClient,
    options: FileSourceOptions,
    assets: Option<Arc<AssetStore>>,
    range_policy: RangePolicy,
}

impl FileDriver {
    pub fn new(
        asset_id: AssetId,
        url: Url,
        net_client: HttpClient,
        options: FileSourceOptions,
        assets: Option<Arc<AssetStore>>,
    ) -> Self {
        let range_policy = RangePolicy::new(true);
        Self {
            asset_id,
            url,
            net_client,
            options: options.clone(),
            assets,
            range_policy,
        }
    }

    pub fn assets(&self) -> Option<Arc<AssetStore>> {
        self.assets.clone()
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub async fn stream(
        &self,
    ) -> Pin<Box<dyn FuturesStream<Item = Result<Bytes, DriverError>> + Send + '_>> {
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
        let s = Engine::new(source, params).into_stream();

        Box::pin(s.filter_map(|item| async move {
            match item {
                Ok(StreamMsg::Data(b)) => Some(Ok(b)),
                Ok(StreamMsg::Control(_)) | Ok(StreamMsg::Event(_)) => None,
                Err(e) => Some(Err(DriverError::from(e))),
            }
        }))
    }

    #[allow(dead_code)]
    /// Seek to a byte position.
    ///
    /// # Contract
    ///
    /// - Validates position against known content size (if known).
    /// - Updates internal range policy state.
    /// - Actual range request implementation is TODO.
    ///
    /// # Errors
    ///
    /// - `InvalidSeekPosition`: when position is beyond known content size.
    pub async fn seek_to(&mut self, position: u64) -> Result<(), DriverError> {
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

// Streaming loops are provided by the generic kithara-stream writer/reader adapters.
// This source only wires them to assets + net.

#[derive(Clone)]
struct FileStream {
    asset_id: AssetId,
    url: Url,
    net_client: HttpClient,
    assets: Option<Arc<AssetStore>>,
    pos: u64,
}

#[derive(Debug, Clone)]
struct FileStreamState {
    cancel: CancellationToken,
    res: AssetResourceType,
}

/// Net adapter over `HttpClient`.
#[derive(Clone)]
struct NetHttp(HttpClient);

impl Net for NetHttp {
    type Request = Url;
    type Error = SourceError;
    type ByteStream = BoxStream<'static, Result<Bytes, SourceError>>;

    fn stream(
        &self,
        req: Self::Request,
    ) -> futures::future::BoxFuture<'static, Result<Self::ByteStream, Self::Error>> {
        let client = self.0.clone();
        Box::pin(async move {
            let s = client.stream(req, None).await.map_err(SourceError::Net)?;
            Ok(s.map(|r| r.map_err(SourceError::Net)).boxed())
        })
    }
}

#[derive(Clone)]
struct AssetSink {
    res: AssetResourceType,
}

impl WriteSink for AssetSink {
    type Error = SourceError;

    fn write_at<'a>(
        &'a self,
        offset: u64,
        data: &'a [u8],
    ) -> futures::future::BoxFuture<'a, Result<(), Self::Error>> {
        let res = self.res.clone();
        Box::pin(async move {
            res.write_at(offset, data)
                .await
                .map_err(SourceError::Storage)
        })
    }

    fn commit<'a>(
        &'a self,
        final_len: Option<u64>,
    ) -> futures::future::BoxFuture<'a, Result<(), Self::Error>> {
        let res = self.res.clone();
        Box::pin(async move { res.commit(final_len).await.map_err(SourceError::Storage) })
    }

    fn fail<'a>(&'a self, msg: String) -> futures::future::BoxFuture<'a, Result<(), Self::Error>> {
        let res = self.res.clone();
        Box::pin(async move { res.fail(msg).await.map_err(SourceError::Storage) })
    }
}

#[derive(Clone)]
struct AssetSource {
    res: AssetResourceType,
}

impl ReadSource for AssetSource {
    type Error = SourceError;

    fn wait_range<'a>(
        &'a self,
        range: Range<u64>,
    ) -> futures::future::BoxFuture<'a, Result<WaitOutcome, Self::Error>> {
        let res = self.res.clone();
        Box::pin(async move {
            match res.wait_range(range).await.map_err(SourceError::Storage)? {
                StorageWaitOutcome::Ready => Ok(WaitOutcome::Ready),
                StorageWaitOutcome::Eof => Ok(WaitOutcome::Eof),
            }
        })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        len: usize,
    ) -> futures::future::BoxFuture<'a, Result<Bytes, Self::Error>> {
        let res = self.res.clone();
        Box::pin(async move { res.read_at(offset, len).await.map_err(SourceError::Storage) })
    }
}

impl EngineSource for FileStream {
    type Error = SourceError;
    type Control = ();
    type Event = ();
    type State = Arc<FileStreamState>;

    fn init(
        &mut self,
        params: StreamParams,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<Self::State, StreamError<Self::Error>>>
                + Send
                + 'static,
        >,
    > {
        let _offline_mode = params.offline_mode;
        let assets = self.assets.clone();
        let url = self.url.clone();

        Box::pin(async move {
            let assets = assets.ok_or_else(|| {
                StreamError::Source(SourceError::Assets(
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "assets store is required for kithara-file streaming; pass Some(AssetStore) to FileSource::open",
                    )
                    .into(),
                ))
            })?;

            let key = (&url).into();
            let cancel = CancellationToken::new();
            let res = assets
                .open_streaming_resource(&key, cancel.clone())
                .await
                .map_err(|e| StreamError::Source(SourceError::Assets(e)))?;

            Ok(Arc::new(FileStreamState { cancel, res }))
        })
    }

    fn open_reader(
        &mut self,
        state: &Self::State,
        params: StreamParams,
    ) -> Result<
        Pin<
            Box<
                dyn FuturesStream<Item = Result<StreamMsg<(), ()>, StreamError<SourceError>>>
                    + Send
                    + 'static,
            >,
        >,
        StreamError<SourceError>,
    > {
        let start_pos = self.pos;
        let _offline_mode = params.offline_mode;
        let state = state.clone();

        Ok(Box::pin(async_stream::stream! {
            let source = AssetSource { res: state.res.clone() };
            let reader_stream = Reader::new(source, start_pos, 64 * 1024).into_stream::<()>();
            pin_mut!(reader_stream);

            while let Some(item) = reader_stream.next().await {
                match item {
                    Ok(StreamMsg::Data(bytes)) => yield Ok(StreamMsg::Data(bytes)),
                    Ok(StreamMsg::Control(_)) | Ok(StreamMsg::Event(_)) => {}
                    Err(StreamError::Source(ReaderError::Wait(storage_err)))
                    | Err(StreamError::Source(ReaderError::Read(storage_err))) => {
                        state.cancel.cancel();
                        match storage_err {
                            SourceError::Storage(e) => {
                                yield Err(StreamError::Source(SourceError::Storage(e)));
                            }
                            other => {
                                yield Err(StreamError::Source(SourceError::Reader(other.to_string())));
                            }
                        }
                        return;
                    }
                    Err(StreamError::Source(other)) => {
                        state.cancel.cancel();
                        yield Err(StreamError::Source(SourceError::Reader(other.to_string())));
                        return;
                    }
                    Err(StreamError::WriterJoin(e)) => {
                        state.cancel.cancel();
                        yield Err(StreamError::Source(SourceError::Reader(e)));
                        return;
                    }
                    Err(StreamError::SeekNotSupported) | Err(StreamError::ChannelClosed) => {
                        state.cancel.cancel();
                        yield Err(StreamError::Source(SourceError::Reader("reader error".to_string())));
                        return;
                    }
                }
            }

            state.cancel.cancel();
        }))
    }

    fn start_writer(
        &mut self,
        state: &Self::State,
        params: StreamParams,
    ) -> Result<WriterTask<SourceError>, StreamError<SourceError>> {
        let _offline_mode = params.offline_mode;
        let net = NetHttp(self.net_client.clone());
        let req = self.url.clone();
        let sink = AssetSink {
            res: state.res.clone(),
        };
        let cancel = state.cancel.clone();

        Ok(tokio::spawn(async move {
            let _ = Writer::new(net, req, sink, cancel).run_with_fail().await;
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
