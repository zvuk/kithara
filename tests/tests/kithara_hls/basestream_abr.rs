//! Basestream ABR switching tests

#![forbid(unsafe_code)]

use std::{sync::Arc, time::Duration};

use axum::{Router, routing::get};
use futures::StreamExt;
use kithara_hls::{
    AbrMode, HlsEvent,
    abr::{AbrConfig, AbrReason, DefaultAbrController},
    fetch::DefaultFetchManager,
    playlist::PlaylistManager,
    stream::{SegmentMeta, SegmentStream, SegmentStreamParams},
};
use kithara_net::HttpClient;
use rstest::rstest;
use tokio::{net::TcpListener, sync::broadcast};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    basestream::build_basestream_with_events,
    fixture::{TestAssets, assets_fixture, net_fixture, test_init_data},
};

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_force_variant_switches_output_and_emits_events(
    assets_fixture: TestAssets,
    net_fixture: HttpClient,
) {
    use super::fixture::TestServer;

    let server = TestServer::new().await;
    let master_url = server.url("/master.m3u8").expect("url");

    let (handle, mut stream, mut events) =
        build_basestream_with_events(master_url, 0, assets_fixture.assets().clone(), net_fixture)
            .await;

    let first = stream.next().await.expect("item").expect("ok");
    assert_eq!(first.variant, 0);

    handle.force_variant(1);

    // Event is emitted after successful variant load, which happens on next()
    let next = stream.next().await.expect("item").expect("ok");
    assert_eq!(next.variant, 1);
    assert!(next.len > 0, "first segment after switch should have data");

    // Check that VariantApplied event was emitted
    let mut saw_applied = false;
    while let Ok(ev) = events.try_recv() {
        if let HlsEvent::VariantApplied {
            from_variant,
            to_variant,
            ..
        } = ev
        {
            assert_eq!(from_variant, 0);
            assert_eq!(to_variant, 1);
            saw_applied = true;
            break;
        }
    }
    assert!(saw_applied, "expected VariantApplied event");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn basestream_emits_abr_events_on_manual_switch(
    assets_fixture: TestAssets,
    net_fixture: HttpClient,
) {
    use super::fixture::TestServer;

    let server = TestServer::new().await;
    let master_url = server.url("/master.m3u8").expect("url");

    let (handle, mut stream, mut events) =
        build_basestream_with_events(master_url, 0, assets_fixture.assets().clone(), net_fixture)
            .await;

    let first = stream.next().await.expect("item").expect("ok");
    assert_eq!(first.variant, 0);

    handle.force_variant(2);

    // Event is emitted after successful variant load, which happens on next()
    let next = stream.next().await.expect("item").expect("ok");
    assert_eq!(next.variant, 2);

    // Check that VariantApplied event was emitted with correct reason
    let mut saw_applied = false;
    while let Ok(ev) = events.try_recv() {
        if let HlsEvent::VariantApplied {
            from_variant,
            to_variant,
            reason,
        } = ev
        {
            assert_eq!(from_variant, 0);
            assert_eq!(to_variant, 2);
            assert_eq!(reason, AbrReason::ManualOverride);
            saw_applied = true;
            break;
        }
    }
    assert!(saw_applied, "expected VariantApplied event");
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

    let fetch = Arc::new(DefaultFetchManager::new(
        assets_fixture.assets().clone(),
        net_fixture,
    ));
    let playlist = Arc::new(PlaylistManager::new(Arc::clone(&fetch), None::<Url>));

    let mut cfg = AbrConfig::default();
    cfg.mode = AbrMode::Auto(Some(2));
    cfg.min_buffer_for_up_switch_secs = 0.0;
    cfg.down_switch_buffer_secs = 0.0;
    cfg.throughput_safety_factor = 1.0;
    cfg.min_switch_interval = Duration::ZERO;

    let abr = DefaultAbrController::new(cfg, None);
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

    let init2 = stream.next().await.unwrap().expect("init v2");
    assert_eq!(init2.variant, 2);
    assert_eq!(init2.segment_index, usize::MAX);

    let seg2 = stream.next().await.unwrap().expect("seg0 v2");
    assert_eq!(seg2.variant, 2);
    assert_eq!(seg2.segment_index, 0);

    let init1 = stream.next().await.unwrap().expect("init v1");
    assert_eq!(init1.variant, 1);
    assert_eq!(init1.segment_index, usize::MAX);

    let seg1 = stream.next().await.unwrap().expect("seg1 v1");
    assert_eq!(seg1.variant, 1);
    assert_eq!(seg1.segment_index, 1);
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

    let fetch = Arc::new(DefaultFetchManager::new(
        assets_fixture.assets().clone(),
        net_fixture,
    ));
    let playlist = Arc::new(PlaylistManager::new(Arc::clone(&fetch), None::<Url>));

    let mut cfg = AbrConfig::default();
    cfg.mode = AbrMode::Auto(Some(2));
    cfg.min_buffer_for_up_switch_secs = 0.0;
    cfg.down_switch_buffer_secs = 0.0;
    cfg.throughput_safety_factor = 1.0;
    cfg.min_switch_interval = Duration::ZERO;

    let abr = DefaultAbrController::new(cfg, None);
    let cancel = CancellationToken::new();
    let (events_tx, _) = broadcast::channel::<HlsEvent>(32);

    let (_handle, base) = SegmentStream::new(SegmentStreamParams {
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
    let mut stream = Box::pin(base);

    let init2: SegmentMeta = stream.next().await.unwrap().expect("init v2");
    assert_eq!(init2.variant, 2);
    assert_eq!(init2.segment_index, usize::MAX);

    let seg2: SegmentMeta = stream.next().await.unwrap().expect("seg0 v2");
    assert_eq!(seg2.variant, 2);
    assert_eq!(seg2.segment_index, 0);

    let init1: SegmentMeta = stream.next().await.unwrap().expect("init v1");
    assert_eq!(init1.variant, 1);
    assert_eq!(init1.segment_index, usize::MAX);

    let seg1: SegmentMeta = stream.next().await.unwrap().expect("seg1 v1");
    assert_eq!(seg1.variant, 1);
    assert_eq!(seg1.segment_index, 1);
}
