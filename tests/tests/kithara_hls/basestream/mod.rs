//! Common helpers for basestream tests

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use kithara_assets::AssetStore;
use kithara_hls::{
    AbrMode, HlsEvent,
    abr::{AbrOptions, DefaultAbrController},
    fetch::DefaultFetchManager,
    playlist::PlaylistManager,
    stream::{PipelineHandle, SegmentMeta, SegmentStream, SegmentStreamParams},
};
use kithara_net::HttpClient;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

pub fn make_fetch_and_playlist(
    assets: AssetStore,
    net: HttpClient,
) -> (Arc<DefaultFetchManager>, Arc<PlaylistManager>) {
    let fetch = Arc::new(DefaultFetchManager::new(assets, net));
    let playlist = Arc::new(PlaylistManager::new(Arc::clone(&fetch), None::<Url>));
    (fetch, playlist)
}

pub fn make_abr(initial_variant: usize) -> DefaultAbrController {
    let mut cfg = AbrOptions::default();
    cfg.mode = AbrMode::Auto(Some(initial_variant));
    DefaultAbrController::new(cfg)
}

pub async fn build_basestream(
    master_url: Url,
    initial_variant: usize,
    assets: AssetStore,
    net: HttpClient,
) -> (PipelineHandle, SegmentStream) {
    let (fetch, playlist) = make_fetch_and_playlist(assets, net);
    let abr = make_abr(initial_variant);
    let cancel = CancellationToken::new();
    let (events_tx, _) = broadcast::channel::<HlsEvent>(32);

    SegmentStream::new(SegmentStreamParams {
        master_url,
        fetch: Arc::clone(&fetch),
        playlist_manager: Arc::clone(&playlist),
        key_manager: None,
        abr_controller: abr,
        events_tx,
        cancel,
        command_capacity: 8,
        min_sample_bytes: 32_000,
    })
}

pub async fn build_basestream_with_events(
    master_url: Url,
    initial_variant: usize,
    assets: AssetStore,
    net: HttpClient,
) -> (PipelineHandle, SegmentStream, broadcast::Receiver<HlsEvent>) {
    let (fetch, playlist) = make_fetch_and_playlist(assets, net);
    let abr = make_abr(initial_variant);
    let cancel = CancellationToken::new();
    let (events_tx, events_rx) = broadcast::channel::<HlsEvent>(32);

    let (handle, stream) = SegmentStream::new(SegmentStreamParams {
        master_url,
        fetch: Arc::clone(&fetch),
        playlist_manager: Arc::clone(&playlist),
        key_manager: None,
        abr_controller: abr,
        events_tx,
        cancel,
        command_capacity: 8,
        min_sample_bytes: 32_000,
    });
    (handle, stream, events_rx)
}

pub async fn collect_all(mut stream: SegmentStream) -> Vec<SegmentMeta> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let payload = item.expect("pipeline should yield Ok payloads");
        out.push(payload);
    }
    out
}
