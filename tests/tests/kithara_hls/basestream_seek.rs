//! Basestream seek operations tests

#![forbid(unsafe_code)]

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use kithara_hls::{HlsEvent, stream::{SegmentStream, SegmentStreamParams}};
use kithara_net::HttpClient;
use rstest::rstest;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::basestream::{build_basestream, make_abr, make_fetch_and_playlist};
use super::fixture::{TestAssets, TestServer, assets_fixture, net_fixture};

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_seek_restarts_from_index(assets_fixture: TestAssets, net_fixture: HttpClient) {
    let server = TestServer::new().await;
    let master_url = server.url("/master.m3u8").expect("url");

    let (handle, mut stream) =
        build_basestream(master_url, 0, assets_fixture.assets().clone(), net_fixture).await;
    let first = stream.next().await.expect("item").expect("ok");
    assert_eq!(first.segment_index, 0);

    handle.seek(2);
    let after_seek = stream.next().await.expect("item").expect("ok");
    assert_eq!(after_seek.segment_index, 2);
    assert_eq!(after_seek.variant, 0);

    let rest: Vec<_> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|res| res.expect("ok"))
        .collect();
    assert!(
        rest.is_empty(),
        "after reading segment 2 there should be no more segments"
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_reconnects_and_resumes_same_segment(
    assets_fixture: TestAssets,
    net_fixture: HttpClient,
) {
    let server = TestServer::new().await;
    let master_url = server.url("/master.m3u8").expect("url");

    let (fetch, playlist) = make_fetch_and_playlist(assets_fixture.assets().clone(), net_fixture);
    let abr1 = make_abr(0);

    let cancel1 = CancellationToken::new();
    let (events_tx1, _) = broadcast::channel::<HlsEvent>(32);
    let (_handle1, mut stream1) = SegmentStream::new(SegmentStreamParams {
        master_url: master_url.clone(),
        fetch: fetch.clone(),
        playlist_manager: playlist.clone(),
        key_manager: None,
        abr_controller: abr1,
        events_tx: events_tx1,
        cancel: cancel1.clone(),
        command_capacity: 8,
        min_sample_bytes: 32_000,
    });

    let first = stream1.next().await.expect("item").expect("ok");
    assert_eq!(first.segment_index, 0);

    cancel1.cancel();
    let ended = stream1.next().await;
    assert!(ended.is_none(), "stream should end after cancellation");

    let abr2 = make_abr(0);
    let cancel2 = CancellationToken::new();
    let (events_tx2, _) = broadcast::channel::<HlsEvent>(32);
    let (handle2, stream2) = SegmentStream::new(SegmentStreamParams {
        master_url,
        fetch,
        playlist_manager: playlist,
        key_manager: None,
        abr_controller: abr2,
        events_tx: events_tx2,
        cancel: cancel2,
        command_capacity: 8,
        min_sample_bytes: 32_000,
    });

    handle2.seek(1);

    let remaining: Vec<_> = stream2
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|res| res.expect("ok"))
        .collect();

    assert_eq!(
        remaining.len(),
        2,
        "should continue from next segment after reconnect"
    );
    assert_eq!(remaining[0].segment_index, 1);
    assert!(
        remaining[0].len > 0,
        "resume should yield segment data after reconnect"
    );
    assert_eq!(remaining[1].segment_index, 2);
}
