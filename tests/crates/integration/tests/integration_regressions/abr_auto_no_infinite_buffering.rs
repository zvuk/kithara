#![cfg(not(target_arch = "wasm32"))]

use std::io::Read;

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::AbrMode,
    hls::{Hls, HlsConfig},
    platform::{CancelToken, time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::DelayRule,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    kithara, rt_cancel, temp_dir,
};

const STORM_ROUNDS: usize = 10;
const VARIANT_COUNT: usize = 3;
const SEGMENTS_PER_VARIANT: usize = 12;
const FIRST_SEGMENT_DELAY_MS: u64 = 2_000;
const BANDWIDTHS: [u64; VARIANT_COUNT] = [64_000, 192_000, 512_000];
const CAPS: [Option<u64>; VARIANT_COUNT] = [Some(96_000), Some(256_000), None];
const READ_DEADLINE: Duration = Duration::from_secs(30);

/// Toggling auto/manual ABR and bandwidth caps while the first
/// segment is buffering must not wedge the stream; a subsequent read must
/// still produce bytes.
#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn abr_mode_storm_does_not_wedge_loading(
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
    #[future(awt)] abr_source: HlsTestServer,
) {
    let server = abr_source;
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        })
        .build();
    let config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(store)
        .pools(pools)
        .cancel(rt_cancel)
        .initial_abr_mode(AbrMode::Auto(None))
        .build();
    let mut stream = Stream::<Hls<TestPools>>::new(config)
        .await
        .expect("create HLS stream");
    let handle = stream
        .abr_handle()
        .expect("HLS stream must expose an ABR handle");
    assert_eq!(
        handle.variants().len(),
        VARIANT_COUNT,
        "precondition: the ABR storm requires three variants"
    );

    for round in 0..STORM_ROUNDS {
        let variant = round % VARIANT_COUNT;
        let mode = if round % 2 == 0 {
            AbrMode::manual(variant)
        } else {
            AbrMode::Auto(None)
        };
        handle
            .set_mode(mode)
            .expect("storm variant must exist in the fixture");
        handle.set_max_bandwidth_bps(CAPS[variant]);
        time::sleep(Duration::from_millis(100)).await;
    }
    handle
        .set_mode(AbrMode::Auto(None))
        .expect("return to automatic ABR");
    handle.set_max_bandwidth_bps(None);

    // The storm lands while the stream is still fetching its delayed first
    // segment — the state the report describes. The assertion stays on the
    // reported symptom: a stream that never yields bytes.
    let bytes = time::timeout(
        READ_DEADLINE,
        spawn_blocking(move || {
            let mut buffer = [0u8; 64 * 1024];
            stream.read(&mut buffer)
        }),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the stream never yielded bytes within {READ_DEADLINE:?} after the \
             ABR mode and bitrate-cap storm — loading is wedged"
        )
    })
    .expect("read task panicked")
    .unwrap_or_else(|error| panic!("read after the ABR storm failed: {error}"));
    assert!(
        bytes > 0,
        "the stream reported end-of-input rather than audio after the ABR storm"
    );
}

#[kithara::fixture]
async fn abr_source() -> HlsTestServer {
    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: VARIANT_COUNT,
        segments_per_variant: SEGMENTS_PER_VARIANT,
        variant_bandwidths: Some(BANDWIDTHS.to_vec()),
        delay_rules: vec![DelayRule {
            segment_eq: Some(0),
            delay_ms: FIRST_SEGMENT_DELAY_MS,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;

    server
}
