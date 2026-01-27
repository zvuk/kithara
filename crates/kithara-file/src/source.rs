#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, asset_root_for_url};
use kithara_net::HttpClient;
use kithara_stream::{OpenedSource, SourceFactory, StreamError, StreamMsg, Writer};
use tokio::sync::broadcast;
use url::Url;

use crate::{
    error::SourceError,
    events::FileEvent,
    inner::File,
    options::FileParams,
    session::{FileStreamState, Progress, SessionSource},
};

impl SourceFactory for File {
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

        let state = FileStreamState::create(
            Arc::new(store),
            net_client.clone(),
            url,
            cancel.clone(),
            params.event_capacity,
        )
        .await
        .map_err(StreamError::Source)?;

        let (events_tx, _) = broadcast::channel(params.event_capacity);
        let progress = Arc::new(Progress::new());

        spawn_download_writer(&net_client, state.clone(), progress.clone());

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

fn spawn_download_writer(
    net_client: &HttpClient,
    state: Arc<FileStreamState>,
    progress: Arc<Progress>,
) {
    let net = net_client.clone();
    let url = state.url().clone();
    let events = state.events().clone();
    let len = state.len();
    let res = state.res().clone();
    let cancel = state.cancel().clone();
    let progress_dl = progress.clone();

    tokio::spawn(async move {
        let writer = Writer::<_, _, FileEvent>::new(net, url, None, res, cancel).with_event(
            move |offset, _len| {
                progress_dl.set_download_pos(offset);
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
    });
}
