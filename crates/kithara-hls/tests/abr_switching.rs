mod fixture;

use std::time::Duration;

use axum::{Router, routing::get};
use fixture::{HlsResult, create_test_cache_and_net};
use futures::StreamExt;
use kithara_hls::{HlsEvent, HlsOptions, HlsSource};
use tokio::net::TcpListener;
use url::Url;

struct AbrTestServer {
    base_url: String,
}

impl AbrTestServer {
    async fn new(master_playlist: String, init: bool, segment0_delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://127.0.0.1:{}", addr.port());

        let master = master_playlist;

        let app =
            Router::new()
                .route(
                    "/master.m3u8",
                    get(move || {
                        let master = master.clone();
                        async move { master }
                    }),
                )
                .route(
                    "/v0.m3u8",
                    get(move || async move { media_playlist(0, init) }),
                )
                .route(
                    "/v1.m3u8",
                    get(move || async move { media_playlist(1, init) }),
                )
                .route(
                    "/v2.m3u8",
                    get(move || async move { media_playlist(2, init) }),
                )
                .route(
                    "/seg/v0_0.bin",
                    get(move || async move {
                        segment_data(0, 0, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v0_1.bin",
                    get(move || async move {
                        segment_data(0, 1, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v0_2.bin",
                    get(move || async move {
                        segment_data(0, 2, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v1_0.bin",
                    get(move || async move {
                        segment_data(1, 0, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v1_1.bin",
                    get(move || async move {
                        segment_data(1, 1, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v1_2.bin",
                    get(move || async move {
                        segment_data(1, 2, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v2_0.bin",
                    get(move || async move { segment_data(2, 0, segment0_delay, 50_000).await }),
                )
                .route(
                    "/seg/v2_1.bin",
                    get(move || async move {
                        segment_data(2, 1, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v2_2.bin",
                    get(move || async move {
                        segment_data(2, 2, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route("/init/v0.bin", get(|| async { init_data(0) }))
                .route("/init/v1.bin", get(|| async { init_data(1) }))
                .route("/init/v2.bin", get(|| async { init_data(2) }));

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self { base_url }
    }

    fn url(&self, path: &str) -> HlsResult<Url> {
        format!("{}{}", self.base_url, path)
            .parse()
            .map_err(|e| kithara_hls::HlsError::InvalidUrl(format!("Invalid test URL: {}", e)))
    }
}

fn master_playlist(v0_bw: u64, v1_bw: u64, v2_bw: u64) -> String {
    format!(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH={v0_bw}
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH={v1_bw}
v1.m3u8
#EXT-X-STREAM-INF:BANDWIDTH={v2_bw}
v2.m3u8
"#
    )
}

fn media_playlist(variant: usize, init: bool) -> String {
    let mut s = String::new();
    s.push_str(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
"#,
    );
    if init {
        s.push_str(&format!("#EXT-X-MAP:URI=\"init/v{}.bin\"\n", variant));
    }
    for i in 0..3 {
        s.push_str("#EXTINF:4.0,\n");
        s.push_str(&format!("seg/v{}_{}.bin\n", variant, i));
    }
    s.push_str("#EXT-X-ENDLIST\n");
    s
}

async fn segment_data(
    variant: usize,
    segment: usize,
    delay: Duration,
    total_len: usize,
) -> Vec<u8> {
    if delay != Duration::ZERO {
        tokio::time::sleep(delay).await;
    }
    let prefix = format!("V{}-SEG-{}:", variant, segment);
    let mut data = prefix.into_bytes();
    if data.len() < total_len {
        data.extend(std::iter::repeat(b'A').take(total_len - data.len()));
    }
    data
}

fn init_data(variant: usize) -> Vec<u8> {
    format!("V{}-INIT:", variant).into_bytes()
}

#[tokio::test]
async fn abr_upswitch_continues_from_current_segment_index() -> HlsResult<()> {
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(1),
    )
    .await;
    let (assets, _net) = create_test_cache_and_net();
    let cache = assets.cache().clone();

    let master_url = server.url("/master.m3u8")?;
    let mut options = HlsOptions::default();
    options.abr_initial_variant_index = Some(0);
    options.abr_min_buffer_for_up_switch = 0.0;
    options.abr_min_switch_interval = Duration::ZERO;

    let session = HlsSource::open(master_url, options, cache).await?;
    let mut events = session.events();

    let mut stream = Box::pin(session.stream());
    let first = stream.next().await.unwrap()?;
    let second = stream.next().await.unwrap()?;

    assert!(first.starts_with(b"V0-SEG-0:"));
    assert!(second.starts_with(b"V2-SEG-1:"));

    let mut seen_decision = false;
    let mut seen_applied = false;
    for _ in 0..50 {
        let ev = tokio::time::timeout(Duration::from_millis(50), events.recv()).await;
        let Ok(Ok(ev)) = ev else { continue };
        match ev {
            HlsEvent::VariantDecision {
                from_variant,
                to_variant,
                ..
            } => {
                if from_variant == 0 && to_variant == 2 {
                    seen_decision = true;
                }
            }
            HlsEvent::VariantApplied {
                from_variant,
                to_variant,
                ..
            } => {
                if from_variant == 0 && to_variant == 2 {
                    seen_applied = true;
                }
            }
            _ => {}
        }
        if seen_decision && seen_applied {
            break;
        }
    }

    assert!(seen_applied, "expected VariantApplied for 0->2 switch");
    Ok(())
}

#[tokio::test]
async fn abr_downswitch_emits_init_before_next_segment_when_required() -> HlsResult<()> {
    let server = AbrTestServer::new(
        master_playlist(500_000, 1_000_000, 5_000_000),
        true,
        Duration::from_millis(200),
    )
    .await;
    let (assets, _net) = create_test_cache_and_net();
    let cache = assets.cache().clone();

    let master_url = server.url("/master.m3u8")?;
    let mut options = HlsOptions::default();
    options.abr_initial_variant_index = Some(2);
    options.abr_min_switch_interval = Duration::ZERO;
    options.abr_min_buffer_for_up_switch = 0.0;

    let session = HlsSource::open(master_url, options, cache).await?;

    let mut stream = Box::pin(session.stream());

    let init0 = stream.next().await.unwrap()?;
    assert!(init0.starts_with(b"V2-INIT:"));

    let seg0 = stream.next().await.unwrap()?;
    assert!(seg0.starts_with(b"V2-SEG-0:"));

    let init_after_switch = stream.next().await.unwrap()?;
    assert!(
        init_after_switch.starts_with(b"V1-INIT:") || init_after_switch.starts_with(b"V0-INIT:"),
        "expected init for downswitched variant"
    );

    let seg1 = stream.next().await.unwrap()?;
    assert!(
        seg1.starts_with(b"V1-SEG-1:") || seg1.starts_with(b"V0-SEG-1:"),
        "expected segment index to continue after downswitch"
    );

    Ok(())
}
