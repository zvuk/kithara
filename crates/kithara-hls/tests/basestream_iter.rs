#![forbid(unsafe_code)]

use std::{sync::Arc, time::Duration};

use axum::{Router, routing::get};
use futures::StreamExt;
use kithara_assets::AssetStore;
use kithara_hls::{
    abr::{AbrConfig, AbrController},
    fetch::FetchManager,
    pipeline::{BaseStream, PipelineEvent, PrefetchStream, SegmentPayload, SegmentStream},
    playlist::PlaylistManager,
};
use kithara_net::HttpClient;
use rstest::rstest;
use tokio::{net::TcpListener, sync::broadcast, time::timeout};
use tokio_util::sync::CancellationToken;
use url::Url;

#[path = "fixture.rs"]
mod fixture;
use fixture::{
    TestAssets, TestServer, assets_fixture, net_fixture, test_init_data, test_segment_data,
};

fn make_fetch_and_playlist(assets: AssetStore, net: HttpClient) -> (FetchManager, PlaylistManager) {
    let fetch = FetchManager::new("test-asset-root".to_string(), assets, net);
    let playlist = PlaylistManager::new(fetch.clone(), None::<Url>);
    (fetch, playlist)
}

fn make_abr(initial_variant: usize) -> AbrController {
    let mut cfg = AbrConfig::default();
    cfg.initial_variant_index = Some(initial_variant);
    AbrController::new(cfg, None)
}

async fn build_basestream(
    master_url: Url,
    initial_variant: usize,
    assets: AssetStore,
    net: HttpClient,
) -> BaseStream {
    let (fetch, playlist) = make_fetch_and_playlist(assets, net);
    let abr = make_abr(initial_variant);
    let cancel = CancellationToken::new();

    BaseStream::build(master_url, Arc::new(fetch), Arc::new(playlist), abr, cancel)
        .await
        .expect("BaseStream::build should succeed")
}

async fn collect_all(mut stream: BaseStream) -> Vec<SegmentPayload> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let payload = item.expect("pipeline should yield Ok payloads");
        out.push(payload);
    }
    out
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_iterates_with_init_and_segments(
    assets_fixture: TestAssets,
    net_fixture: HttpClient,
) {
    let server = TestServer::new().await;
    let master_url = server.url("/master-init.m3u8").expect("url");

    let stream =
        build_basestream(master_url, 0, assets_fixture.assets().clone(), net_fixture).await;
    let items = collect_all(stream).await;

    assert_eq!(items.len(), 4, "init + 3 segments expected");
    assert_eq!(items[0].meta.segment_index, usize::MAX, "init first");
    assert_eq!(items[0].meta.variant, 0);
    assert!(
        items[0].bytes.starts_with(&test_init_data(0)),
        "init bytes should match fixture"
    );

    for (idx, payload) in items.iter().enumerate().skip(1) {
        assert_eq!(payload.meta.variant, 0);
        assert_eq!(payload.meta.segment_index, idx - 1);
        assert!(
            payload.bytes.starts_with(&test_segment_data(0, idx - 1)),
            "segment {} should match fixture",
            idx - 1
        );
    }
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_seek_restarts_from_index(assets_fixture: TestAssets, net_fixture: HttpClient) {
    let server = TestServer::new().await;
    let master_url = server.url("/master.m3u8").expect("url");

    let mut stream =
        build_basestream(master_url, 0, assets_fixture.assets().clone(), net_fixture).await;
    let first = stream.next().await.expect("item").expect("ok");
    assert_eq!(first.meta.segment_index, 0);

    stream.seek(2);
    let after_seek = stream.next().await.expect("item").expect("ok");
    assert_eq!(after_seek.meta.segment_index, 2);
    assert_eq!(after_seek.meta.variant, 0);

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
async fn basestream_force_variant_switches_output_and_emits_events(
    assets_fixture: TestAssets,
    net_fixture: HttpClient,
) {
    let server = TestServer::new().await;
    let master_url = server.url("/master.m3u8").expect("url");

    let mut stream =
        build_basestream(master_url, 0, assets_fixture.assets().clone(), net_fixture).await;
    let mut events: broadcast::Receiver<PipelineEvent> = stream.event_sender().subscribe();

    let first = stream.next().await.expect("item").expect("ok");
    assert_eq!(first.meta.variant, 0);

    stream.force_variant(1);

    let ev = timeout(Duration::from_millis(200), async {
        loop {
            match events.recv().await {
                Ok(PipelineEvent::VariantSelected { from, to, .. }) => break (from, to),
                Ok(_) => continue,
                Err(e) => panic!("events channel closed: {e}"),
            }
        }
    })
    .await
    .expect("expected VariantSelected within timeout");
    assert_eq!(ev.0, 0);
    assert_eq!(ev.1, 1);

    let next = stream.next().await.expect("item").expect("ok");
    assert_eq!(next.meta.variant, 1);
    assert!(
        next.bytes.starts_with(&test_segment_data(1, 0)),
        "first segment after switch should be from variant 1"
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_stops_after_cancellation(assets_fixture: TestAssets, net_fixture: HttpClient) {
    let server = TestServer::new().await;
    let master_url = server.url("/master.m3u8").expect("url");

    let (fetch, playlist) = make_fetch_and_playlist(assets_fixture.assets().clone(), net_fixture);
    let abr = make_abr(0);
    let cancel = CancellationToken::new();

    let mut stream = BaseStream::build(
        master_url,
        Arc::new(fetch),
        Arc::new(playlist),
        abr,
        cancel.clone(),
    )
    .await
    .expect("build");

    let first = stream.next().await.expect("item").expect("ok");
    assert_eq!(first.meta.segment_index, 0);
    cancel.cancel();

    let next = stream.next().await;
    assert!(next.is_none(), "stream should stop after cancellation");
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

    let mut stream =
        build_basestream(master_url, 0, assets_fixture.assets().clone(), net_fixture).await;

    let first = stream.next().await.expect("item").expect("ok");
    assert_eq!(first.meta.segment_index, 0);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut remaining = Vec::new();
    while let Some(item) = stream.next().await {
        remaining.push(item.expect("ok"));
    }

    assert_eq!(remaining.len(), 2, "should receive remaining segments");
    assert_eq!(remaining[0].meta.segment_index, 1);
    assert_eq!(remaining[1].meta.segment_index, 2);
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

    let (fetch_mgr, playlist_mgr) =
        make_fetch_and_playlist(assets_fixture.assets().clone(), net_fixture);
    let fetch = Arc::new(fetch_mgr);
    let playlist = Arc::new(playlist_mgr);
    let abr = make_abr(0);

    let cancel1 = CancellationToken::new();
    let mut stream1 = BaseStream::build(
        master_url.clone(),
        fetch.clone(),
        playlist.clone(),
        abr.clone(),
        cancel1.clone(),
    )
    .await
    .expect("build");

    let first = stream1.next().await.expect("item").expect("ok");
    assert_eq!(first.meta.segment_index, 0);

    cancel1.cancel();
    let ended = stream1.next().await;
    assert!(ended.is_none(), "stream should end after cancellation");

    let cancel2 = CancellationToken::new();
    let mut stream2 = BaseStream::build(master_url, fetch, playlist, abr, cancel2)
        .await
        .expect("rebuild");

    stream2.seek(1);

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
    assert_eq!(remaining[0].meta.segment_index, 1);
    assert!(
        remaining[0].bytes.starts_with(&test_segment_data(0, 1)),
        "resume should yield the same segment data after reconnect"
    );
    assert_eq!(remaining[1].meta.segment_index, 2);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_downswitch_emits_init_before_next_segment(
    assets_fixture: TestAssets,
    net_fixture: HttpClient,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    let master = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=500000
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1000000
v1.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5000000
v2.m3u8
"#;

    fn media_playlist(variant: usize) -> String {
        format!(
            r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MAP:URI="init/v{}.bin"
#EXTINF:4.0,
seg/v{}_0.bin
#EXTINF:4.0,
seg/v{}_1.bin
#EXTINF:4.0,
seg/v{}_2.bin
#EXT-X-ENDLIST
"#,
            variant, variant, variant, variant
        )
    }

    async fn segment_bytes(
        variant: usize,
        segment: usize,
        delay: Duration,
        total_len: usize,
    ) -> Vec<u8> {
        if delay != Duration::ZERO {
            tokio::time::sleep(delay).await;
        }
        let mut data = format!("V{}-SEG-{}:", variant, segment).into_bytes();
        if data.len() < total_len {
            data.extend(std::iter::repeat(b'A').take(total_len - data.len()));
        }
        data
    }

    let app = Router::new()
        .route(
            "/master.m3u8",
            get({
                let master = master.to_string();
                move || {
                    let master = master.clone();
                    async move { master }
                }
            }),
        )
        .route("/v0.m3u8", get(|| async move { media_playlist(0) }))
        .route("/v1.m3u8", get(|| async move { media_playlist(1) }))
        .route("/v2.m3u8", get(|| async move { media_playlist(2) }))
        .route("/init/v0.bin", get(|| async { test_init_data(0) }))
        .route("/init/v1.bin", get(|| async { test_init_data(1) }))
        .route("/init/v2.bin", get(|| async { test_init_data(2) }))
        .route(
            "/seg/v0_0.bin",
            get(|| async move { segment_bytes(0, 0, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v0_1.bin",
            get(|| async move { segment_bytes(0, 1, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v0_2.bin",
            get(|| async move { segment_bytes(0, 2, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v1_0.bin",
            get(|| async move { segment_bytes(1, 0, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v1_1.bin",
            get(|| async move { segment_bytes(1, 1, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v1_2.bin",
            get(|| async move { segment_bytes(1, 2, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v2_0.bin",
            get(|| async move { segment_bytes(2, 0, Duration::from_millis(200), 50_000).await }),
        )
        .route(
            "/seg/v2_1.bin",
            get(|| async move { segment_bytes(2, 1, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v2_2.bin",
            get(|| async move { segment_bytes(2, 2, Duration::from_millis(1), 200_000).await }),
        );

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let master_url: Url = format!("{}/master.m3u8", base_url)
        .parse()
        .expect("valid master url");

    let asset_root = format!("basestream-downswitch-{}", addr.port());
    let fetch = FetchManager::new(asset_root, assets_fixture.assets().clone(), net_fixture);
    let playlist = PlaylistManager::new(fetch.clone(), None::<Url>);

    let mut cfg = AbrConfig::default();
    cfg.initial_variant_index = Some(2);
    cfg.min_buffer_for_up_switch_secs = 0.0;
    cfg.down_switch_buffer_secs = 0.0;
    cfg.throughput_safety_factor = 1.0;
    cfg.min_switch_interval = Duration::ZERO;

    let abr = AbrController::new(cfg, None);
    let cancel = CancellationToken::new();

    let mut stream =
        BaseStream::build(master_url, Arc::new(fetch), Arc::new(playlist), abr, cancel)
            .await
            .expect("build basestream");

    let init2 = stream.next().await.unwrap().expect("init v2");
    assert_eq!(init2.meta.variant, 2);
    assert_eq!(init2.meta.segment_index, usize::MAX);

    let seg2 = stream.next().await.unwrap().expect("seg0 v2");
    assert_eq!(seg2.meta.variant, 2);
    assert_eq!(seg2.meta.segment_index, 0);

    let init1 = stream.next().await.unwrap().expect("init v1");
    assert_eq!(init1.meta.variant, 1);
    assert_eq!(init1.meta.segment_index, usize::MAX);

    let seg1 = stream.next().await.unwrap().expect("seg1 v1");
    assert_eq!(seg1.meta.variant, 1);
    assert_eq!(seg1.meta.segment_index, 1);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_prefetch_downswitch_preserves_order(
    assets_fixture: TestAssets,
    net_fixture: HttpClient,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    let master = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=500000
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1000000
v1.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5000000
v2.m3u8
"#;

    fn media_playlist(variant: usize) -> String {
        format!(
            r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MAP:URI="init/v{}.bin"
#EXTINF:4.0,
seg/v{}_0.bin
#EXTINF:4.0,
seg/v{}_1.bin
#EXTINF:4.0,
seg/v{}_2.bin
#EXT-X-ENDLIST
"#,
            variant, variant, variant, variant
        )
    }

    async fn segment_bytes(
        variant: usize,
        segment: usize,
        delay: Duration,
        total_len: usize,
    ) -> Vec<u8> {
        if delay != Duration::ZERO {
            tokio::time::sleep(delay).await;
        }
        let mut data = format!("V{}-SEG-{}:", variant, segment).into_bytes();
        if data.len() < total_len {
            data.extend(std::iter::repeat(b'A').take(total_len - data.len()));
        }
        data
    }

    let app = Router::new()
        .route(
            "/master.m3u8",
            get({
                let master = master.to_string();
                move || {
                    let master = master.clone();
                    async move { master }
                }
            }),
        )
        .route("/v0.m3u8", get(|| async move { media_playlist(0) }))
        .route("/v1.m3u8", get(|| async move { media_playlist(1) }))
        .route("/v2.m3u8", get(|| async move { media_playlist(2) }))
        .route("/init/v0.bin", get(|| async { test_init_data(0) }))
        .route("/init/v1.bin", get(|| async { test_init_data(1) }))
        .route("/init/v2.bin", get(|| async { test_init_data(2) }))
        .route(
            "/seg/v0_0.bin",
            get(|| async move { segment_bytes(0, 0, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v0_1.bin",
            get(|| async move { segment_bytes(0, 1, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v0_2.bin",
            get(|| async move { segment_bytes(0, 2, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v1_0.bin",
            get(|| async move { segment_bytes(1, 0, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v1_1.bin",
            get(|| async move { segment_bytes(1, 1, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v1_2.bin",
            get(|| async move { segment_bytes(1, 2, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v2_0.bin",
            get(|| async move { segment_bytes(2, 0, Duration::from_millis(200), 50_000).await }),
        )
        .route(
            "/seg/v2_1.bin",
            get(|| async move { segment_bytes(2, 1, Duration::from_millis(1), 200_000).await }),
        )
        .route(
            "/seg/v2_2.bin",
            get(|| async move { segment_bytes(2, 2, Duration::from_millis(1), 200_000).await }),
        );

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let master_url: Url = format!("{}/master.m3u8", base_url)
        .parse()
        .expect("valid master url");

    let asset_root = format!("basestream-prefetch-downswitch-{}", addr.port());
    let fetch = FetchManager::new(asset_root, assets_fixture.assets().clone(), net_fixture);
    let playlist = PlaylistManager::new(fetch.clone(), None::<Url>);

    let mut cfg = AbrConfig::default();
    cfg.initial_variant_index = Some(2);
    cfg.min_buffer_for_up_switch_secs = 0.0;
    cfg.down_switch_buffer_secs = 0.0;
    cfg.throughput_safety_factor = 1.0;
    cfg.min_switch_interval = Duration::ZERO;

    let abr = AbrController::new(cfg, None);
    let cancel = CancellationToken::new();

    let base = BaseStream::build(
        master_url,
        Arc::new(fetch),
        Arc::new(playlist),
        abr,
        cancel.clone(),
    )
    .await
    .expect("build basestream");
    let mut stream = PrefetchStream::new(base, 1, cancel);

    let init2 = stream.next().await.unwrap().expect("init v2");
    assert_eq!(init2.meta.variant, 2);
    assert_eq!(init2.meta.segment_index, usize::MAX);

    let seg2 = stream.next().await.unwrap().expect("seg0 v2");
    assert_eq!(seg2.meta.variant, 2);
    assert_eq!(seg2.meta.segment_index, 0);

    let init1 = stream.next().await.unwrap().expect("init v1");
    assert_eq!(init1.meta.variant, 1);
    assert_eq!(init1.meta.segment_index, usize::MAX);

    let seg1 = stream.next().await.unwrap().expect("seg1 v1");
    assert_eq!(seg1.meta.variant, 1);
    assert_eq!(seg1.meta.segment_index, 1);
}
