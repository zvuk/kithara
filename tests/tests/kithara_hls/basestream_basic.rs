//! Basic basestream iteration tests

#![forbid(unsafe_code)]

use std::time::Duration;

use futures::StreamExt;
use kithara_hls::stream::SegmentStream;
use kithara_net::HttpClient;
use rstest::rstest;
use tokio_util::sync::CancellationToken;

use super::basestream::{build_basestream, collect_all, make_abr, make_fetch_and_playlist};
use super::fixture::{TestAssets, TestServer, assets_fixture, net_fixture};

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_iterates_with_init_and_segments(
    assets_fixture: TestAssets,
    net_fixture: HttpClient,
) {
    let server = TestServer::new().await;
    let master_url = server.url("/master-init.m3u8").expect("url");

    let (_handle, stream) =
        build_basestream(master_url, 0, assets_fixture.assets().clone(), net_fixture).await;
    let items = collect_all(stream).await;

    assert_eq!(items.len(), 4, "init + 3 segments expected");
    assert_eq!(items[0].segment_index, usize::MAX, "init first");
    assert_eq!(items[0].variant, 0);
    assert!(items[0].len > 0, "init segment should have non-zero length");

    for (idx, meta) in items.iter().enumerate().skip(1) {
        assert_eq!(meta.variant, 0);
        assert_eq!(meta.segment_index, idx - 1);
        assert!(
            meta.len > 0,
            "segment {} should have non-zero length",
            idx - 1
        );
    }
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_pause_and_resume_continues_streaming(
    assets_fixture: TestAssets,
    net_fixture: HttpClient,
) {
    let server = TestServer::new().await;
    let master_url = server.url("/master.m3u8").expect("url");

    let (_handle, mut stream) =
        build_basestream(master_url, 0, assets_fixture.assets().clone(), net_fixture).await;

    let first = stream.next().await.expect("item").expect("ok");
    assert_eq!(first.segment_index, 0);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut remaining = Vec::new();
    while let Some(item) = stream.next().await {
        remaining.push(item.expect("ok"));
    }

    assert_eq!(remaining.len(), 2, "should receive remaining segments");
    assert_eq!(remaining[0].segment_index, 1);
    assert_eq!(remaining[1].segment_index, 2);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_stops_after_cancellation(assets_fixture: TestAssets, net_fixture: HttpClient) {
    use kithara_hls::{HlsEvent, stream::{SegmentStreamParams}};
    use std::sync::Arc;
    use tokio::sync::broadcast;

    let server = TestServer::new().await;
    let master_url = server.url("/master.m3u8").expect("url");

    let (fetch, playlist) = make_fetch_and_playlist(assets_fixture.assets().clone(), net_fixture);
    let abr = make_abr(0);
    let cancel = CancellationToken::new();
    let (events_tx, _) = broadcast::channel::<HlsEvent>(32);

    let (_handle, mut stream) = SegmentStream::new(SegmentStreamParams {
        master_url,
        fetch: Arc::clone(&fetch),
        playlist_manager: Arc::clone(&playlist),
        key_manager: None,
        abr_controller: abr,
        events_tx,
        cancel: cancel.clone(),
        command_capacity: 8,
        min_sample_bytes: 32_000,
    });

    let first = stream.next().await.expect("item").expect("ok");
    assert_eq!(first.segment_index, 0);
    cancel.cancel();

    let next = stream.next().await;
    assert!(next.is_none(), "stream should stop after cancellation");
}
