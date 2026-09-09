#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    decode::DecoderBackend,
    events::{AbrMode, VariantInfo},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, sleep},
    },
    play::{PlayWorker, PlayWorkerConfig, Resource},
    queue::TrackSource,
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{
    config::{AppConfig, AppDrm},
    document::Config,
    pools::{PoolsSection, build as app_pools},
};
use kithara_integration_tests::{
    TestTempDir, bufpool_ext::pools as test_pools, kithara, offline::OfflinePlayer,
};
use tracing::info;

/// Production zvuk DRM master from the on-device AAC->FLAC recreate trace.
/// The baked `zvuk-prod` provider supplies the DRM keyserver headers.
const PROD_TRACK: &str = "https://cdn-hls-slicer.zvuk.com/drm/track/125895892_2/master.m3u8";
const TRACK_NAME: &str = "zvuk_prod_aac_to_flac_switch";
const START_VARIANT: usize = 0;
const OUT_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const RENDER_BATCH_BLOCKS: u32 = 16;
const PRE_SWITCH_POSITION_SECS: f64 = 2.2;
const POST_SWITCH_MIN_GAIN_SECS: f64 = 1.0;
const POSITION_EPS_SECS: f64 = 0.001;
const STALL_TICK: Duration = Duration::from_millis(25);
const MAX_STALL_TICKS: u32 = 240;

#[derive(Debug, Clone, Copy)]
struct PositionWindow {
    start: f64,
    end: f64,
    rendered_blocks: u32,
}

fn contains_flac(value: &Option<String>) -> bool {
    value
        .as_deref()
        .is_some_and(|s| s.to_ascii_lowercase().contains("flac"))
}

fn find_flac_variant_idx(variants: &[VariantInfo]) -> usize {
    variants
        .iter()
        .find(|variant| contains_flac(&variant.codecs) || contains_flac(&variant.container))
        .or_else(|| {
            variants
                .iter()
                .max_by_key(|variant| variant.bandwidth_bps.unwrap_or(0))
        })
        .map_or_else(
            || panic!("no FLAC/top variant among {} variants", variants.len()),
            |variant| variant.variant_index.get(),
        )
}

async fn render_until<F>(player: &mut OfflinePlayer, label: &str, mut done: F) -> PositionWindow
where
    F: FnMut(f64) -> bool,
{
    let start = player.position();
    let mut last = start;
    let mut rendered_blocks = 0u32;
    let mut stall_ticks = 0u32;

    loop {
        for _ in 0..RENDER_BATCH_BLOCKS {
            let _ = player.render(BLOCK_FRAMES).await;
            rendered_blocks = rendered_blocks.saturating_add(1);
        }

        let position = player.position();
        assert!(
            position + POSITION_EPS_SECS >= last,
            "{label}: playback position regressed from {last:.3}s to {position:.3}s"
        );

        if done(position) {
            return PositionWindow {
                start,
                end: position,
                rendered_blocks,
            };
        }

        if position > last {
            last = position;
            stall_ticks = 0;
        } else {
            stall_ticks = stall_ticks.saturating_add(1);
            assert!(
                stall_ticks <= MAX_STALL_TICKS,
                "{label}: target condition was not met; playback stalled at \
                 {position:.3}s after {rendered_blocks} rendered blocks"
            );
            sleep(STALL_TICK).await;
        }
    }
}

/// Production RED repro for the Apple decoder recreate path:
/// start on zvuk's low AAC HE-AAC v2 variant, play real AAC for a couple of
/// seconds, then force a runtime switch to the FLAC lossless variant. Standalone
/// FLAC from startup is known green; this test isolates the AAC decoder teardown
/// plus FLAC decoder construction path.
#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn zvuk_prod_aac_to_flac_switch(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let pools = app_pools(&PoolsSection::default()).expect("build app pool region");
    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), CancelToken::never()))
            .build(),
    );
    let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
    let shutdown = CancelToken::never();
    let document = Config::load(None, None).expect("the shipped configuration loads");
    let store = AssetStore::builder(pools.clone())
        .cancel(shutdown.child())
        .backend(StorageBackend::default())
        .flush_hub(flush_hub)
        .layouts(document.asset_layouts())
        .build();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools)
            .cancel(shutdown.child())
            .build(),
    );
    let config = AppConfig::builder()
        .drm(AppDrm::new(
            document
                .drm_policy()
                .expect("the shipped providers are valid"),
        ))
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker)
        .store(store)
        .build();
    let temp = TestTempDir::new();

    let TrackSource::Config(cfg) = super::source_helper::app_track_source(
        PROD_TRACK,
        &config,
        super::source_helper::app_disk_asset_store(&config, temp.path()),
        backend,
        AbrMode::manual(START_VARIANT),
        Some(TRACK_NAME),
    ) else {
        panic!("expected an HLS config source for {PROD_TRACK}");
    };

    let resource = Resource::new(*cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed [{PROD_TRACK}]: {e:?}"));
    let abr = resource
        .abr_handle()
        .unwrap_or_else(|| panic!("HLS resource exposes no ABR handle for {PROD_TRACK}"));
    let variants = abr.variants();
    let flac_idx = find_flac_variant_idx(&variants);
    assert_ne!(
        flac_idx, START_VARIANT,
        "FLAC/top variant discovery selected the AAC start variant; variants={variants:?}"
    );
    assert_eq!(
        abr.current_variant_index(),
        Some(START_VARIANT),
        "resource must start locked on low AAC variant {START_VARIANT}"
    );

    info!(
        variant_count = variants.len(),
        flac_idx, "loaded zvuk prod HLS master"
    );

    let mut player = OfflinePlayer::new(
        HostConfig::offline(test_pools())
            .sample_rate(NonZeroU32::new(OUT_RATE).expect("output rate is non-zero"))
            .build(),
    )
    .await;
    player.load_and_fadein(resource).await;

    let pre = render_until(&mut player, "AAC warmup before FLAC switch", |position| {
        position >= PRE_SWITCH_POSITION_SECS
    })
    .await;
    assert!(
        pre.end >= PRE_SWITCH_POSITION_SECS,
        "AAC warmup must advance to at least {PRE_SWITCH_POSITION_SECS:.1}s; \
         rendered_blocks={}, window={pre:?}",
        pre.rendered_blocks
    );

    abr.set_mode(AbrMode::manual(flac_idx))
        .unwrap_or_else(|e| panic!("pin FLAC variant {flac_idx} failed: {e:?}"));
    info!(
        flac_idx,
        pre_position = pre.end,
        pre_rendered_blocks = pre.rendered_blocks,
        "forced runtime AAC->FLAC variant switch"
    );

    let applied = render_until(&mut player, "FLAC variant apply", |_| {
        abr.current_variant_index() == Some(flac_idx)
    })
    .await;
    assert_eq!(
        abr.current_variant_index(),
        Some(flac_idx),
        "runtime switch must commit FLAC variant {flac_idx}; applied_window={applied:?}"
    );
    info!(
        flac_idx,
        applied_position = applied.end,
        applied_rendered_blocks = applied.rendered_blocks,
        "FLAC variant applied"
    );

    let post_target = player.position() + POST_SWITCH_MIN_GAIN_SECS;
    let post = render_until(&mut player, "post-switch FLAC", |position| {
        position >= post_target
    })
    .await;
    let post_gain = post.end - post.start;
    assert!(
        post_gain >= POST_SWITCH_MIN_GAIN_SECS,
        "playback must keep advancing after AAC->FLAC switch; \
         gained {post_gain:.3}s, expected >= {POST_SWITCH_MIN_GAIN_SECS:.1}s, \
         rendered_blocks={}, window={post:?}",
        post.rendered_blocks
    );
    player.close().await;
}
