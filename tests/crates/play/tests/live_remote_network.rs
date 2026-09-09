#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! The two `resource_regressions` cases that read a real remote stream instead
//! of a fixture: one through `Resource` directly, one through the full
//! `PlayerImpl` flow the GUI uses. Their local mirrors stay in
//! `resource_regressions.rs`; these need the internet, and the `zvq.me` URLs
//! need the corporate VPN on top.
//!
//! Compiled only into `suite_network`, which needs the `network` feature.
use std::num::NonZeroUsize;

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::ReadOutcome,
    decode::DecoderBackend,
    events::TrackId,
    host::{Host, HostConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{self, Duration, Instant},
    },
    play::{
        PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, Resource, ResourceConfig,
        ResourceSrc, SelectTransition,
    },
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{TestTempDir, temp_dir};
use tracing::debug;

use crate::bufpool_ext::{Pools, TestPools, pools};

fn asset_store(temp_dir: &TestTempDir, ephemeral: bool, pools: Pools) -> AssetStore<TestPools> {
    if ephemeral {
        AssetStore::builder(pools)
            .backend(StorageBackend::Memory)
            .cache_capacity(NonZeroUsize::new(4).expect("nonzero"))
            .max_assets(8)
            .build()
    } else {
        AssetStore::builder(pools)
            .backend(StorageBackend::Disk {
                root: temp_dir.path().to_path_buf(),
            })
            .build()
    }
}

/// Live remote streams through `ResourceConfig` — same code path as kithara-app.
/// No hint, no extension manipulation — exactly what the app does.
///
/// Requires internet (silvercomet) and corporate VPN (zvuk).
// flash(false): live-internet sockets are invisible to the flash engine; virtual
// sleep/deadline would outrun the real download and fail spuriously.
#[kithara::test(
    tokio,
    flash(false),
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(10)
)]
#[case::silvercomet_mp3_symphonia(
    "https://stream.silvercomet.top/track.mp3",
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_mp3_apple("https://stream.silvercomet.top/track.mp3", DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::silvercomet_mp3_android(
        "https://stream.silvercomet.top/track.mp3",
        DecoderBackend::Android
    )
)]
#[case::silvercomet_hls_symphonia(
    "https://stream.silvercomet.top/hls/master.m3u8",
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_hls_apple(
        "https://stream.silvercomet.top/hls/master.m3u8",
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    target_os = "android",
    case::silvercomet_hls_android(
        "https://stream.silvercomet.top/hls/master.m3u8",
        DecoderBackend::Android
    )
)]
#[case::zvuk_27390231_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_27390231_apple(
        "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_27390231_android(
        "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
        DecoderBackend::Android
    )
)]
#[case::zvuk_151585912_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_151585912_apple(
        "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_151585912_android(
        "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
        DecoderBackend::Android
    )
)]
#[case::zvuk_125475417_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_125475417_apple(
        "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_125475417_android(
        "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
        DecoderBackend::Android
    )
)]
async fn live_remote_resource_decodes_with_duration(
    #[case] url: &str,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let pools = pools();
    let store = asset_store(&temp_dir, true, pools.clone());
    let net = NetOptions::builder()
        .inactivity_timeout(Duration::from_secs(25))
        .build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), CancelToken::never()))
            .build(),
    );
    let config: ResourceConfig<TestPools> =
        ResourceConfig::for_src(ResourceSrc::parse(url).expect("valid URL"))
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools).build()))
            .store(store)
            .downloader(downloader)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .build();

    let mut resource = Box::pin(Resource::new(config))
        .await
        .unwrap_or_else(|e| panic!("{url}: Resource::new failed: {e}"));

    let duration = resource.duration();
    assert!(
        duration.is_some(),
        "{url}: duration must be reported (got None)"
    );
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        dur_secs > 30.0,
        "{url}: expected duration > 30s for a real track, got {dur_secs:.1}s"
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut samples = 0usize;
    let mut buf = [0.0f32; 4096];
    loop {
        match resource.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => {
                let count = count.get();
                if count > 0 {
                    samples += count;
                }
            }
            Ok(ReadOutcome::Eof { .. }) => break,
            Ok(ReadOutcome::Pending { .. }) => {}
            Err(e) => panic!("{url}: decode error: {e}"),
        }
        if resource.position() >= Duration::from_secs(2) {
            break;
        }
        assert!(
            Instant::now() <= deadline,
            "{url}: timed out waiting for PCM data (pos={:?}, samples={samples})",
            resource.position()
        );
        time::sleep(Duration::from_millis(5)).await;
    }

    assert!(samples > 0, "{url}: must decode PCM samples");
    assert!(
        resource.position() >= Duration::from_secs(2),
        "{url}: must decode at least 2s, got {:?}",
        resource.position()
    );
}

/// Reproduces EXACTLY the app flow: `PlayerImpl` + `prepare_config` + `Resource::new` +
/// `select_item` + `duration_seconds()`. This is what the GUI reads.
// flash(false): live-internet sockets are invisible to the flash engine; a virtual
// 500ms pacing sleep would elapse before the real metadata fetch completes.
#[kithara::test(
    tokio,
    flash(false),
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(10)
)]
#[case::silvercomet_mp3_symphonia(
    "https://stream.silvercomet.top/track.mp3",
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::silvercomet_mp3_apple("https://stream.silvercomet.top/track.mp3", DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::silvercomet_mp3_android(
        "https://stream.silvercomet.top/track.mp3",
        DecoderBackend::Android
    )
)]
#[case::zvuk_125475417_symphonia(
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    DecoderBackend::Symphonia
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::zvuk_125475417_apple(
        "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
        DecoderBackend::Apple
    )
)]
#[cfg_attr(
    target_os = "android",
    case::zvuk_125475417_android(
        "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
        DecoderBackend::Android
    )
)]
async fn player_mp3_duration_matches_app_flow(
    #[case] url: &str,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let pools = pools();
    let store = asset_store(&temp_dir, true, pools.clone());

    let mut host = Host::new(HostConfig::builder().build()).expect("create playback host");
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(host.requested_sample_rate())
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools).build()))
            .build(),
    );
    let player = host
        .insert(player)
        .expect("insert player into playback host");
    player.reserve_slots(1);

    let mut config: ResourceConfig<TestPools> =
        ResourceConfig::for_src(ResourceSrc::parse(url).unwrap())
            .store(store)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .build();
    config = player
        .prepare_config(config)
        .expect("prepare live remote resource config");

    let resource = Box::pin(Resource::new(config))
        .await
        .unwrap_or_else(|e| panic!("{url}: Resource::new failed: {e}"));

    player
        .replace_item(0, resource, TrackId::allocate())
        .expect("install live remote resource");
    player
        .select_item_with_crossfade(
            0,
            SelectTransition {
                autoplay: true,
                crossfade_seconds: player.crossfade_duration(),
            },
        )
        .expect("select live remote resource");

    // Wait on the concrete state the assertion below reads: the selected
    // slot's duration committed into player shared state. The inner sleep is
    // only the poll cadence of this state-checking loop; the test-level
    // `timeout(30s)` bounds a genuine stall.
    while player.duration_seconds().is_none() {
        time::sleep(Duration::from_millis(20)).await;
    }

    let dur = player.duration_seconds();
    debug!("{url} duration_seconds={dur:?}");
    assert!(
        dur.is_some(),
        "{url}: player.duration_seconds() must not be None"
    );
    let dur_secs = dur.expect("checked");
    assert!(dur_secs > 30.0, "{url}: expected >30s, got {dur_secs:.1}s");
    host.remove(&player).expect("remove live remote player");
}
