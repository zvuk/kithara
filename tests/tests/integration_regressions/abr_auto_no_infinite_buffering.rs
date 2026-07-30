#![cfg(not(target_arch = "wasm32"))]

use std::io::Read;

use kithara::{
    events::AbrMode,
    hls::{Hls, HlsConfig},
    platform::{CancelToken, time::Duration, tokio::task::spawn_blocking},
    stream::{Stream, VariantChangeError},
};
use kithara_integration_tests::{
    TestTempDir,
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
const RETRY_BACKOFF: Duration = Duration::from_millis(50);

fn is_variant_fence(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.downcast_ref::<VariantChangeError>().is_some())
}

/// Toggling auto/manual ABR and bandwidth caps while the first
/// segment is buffering must not wedge the stream; a subsequent read must
/// still produce bytes.
#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn abr_mode_storm_does_not_wedge_loading(temp_dir: TestTempDir, rt_cancel: CancelToken) {
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

    let config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
        .cancel(rt_cancel)
        .initial_abr_mode(AbrMode::Auto(None))
        .build();
    let mut stream = Stream::<Hls>::new(config).await.expect("create HLS stream");
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
    // segment — the state the report describes. `VariantChangeError` is the
    // documented cross-variant fence, not a failure: the fixture declares no
    // `CODECS`, so every ABR commit takes the decoder-recreate branch, and the
    // gate stays shut until the consumer acks it with `clear_variant_fence`.
    // Retrying without that ack deadlocks the reader, not the framework.
    // Acking and reading on keeps the assertion on the reported symptom, a
    // stream that never yields bytes.
    let read = time::timeout(READ_DEADLINE, async move {
        loop {
            let attempt = spawn_blocking(move || {
                let mut buffer = [0u8; 64 * 1024];
                let outcome = stream.read(&mut buffer);
                (stream, outcome)
            })
            .await
            .expect("read task panicked");
            stream = attempt.0;
            match attempt.1 {
                Ok(bytes) => return Ok(bytes),
                Err(error) if is_variant_fence(&error) => {
                    stream.clear_variant_fence();
                }
                Err(error) => return Err(error),
            }
            time::sleep(RETRY_BACKOFF).await;
        }
    })
    .await;

    let bytes = read
        .unwrap_or_else(|_| {
            panic!(
                "the stream never yielded bytes within {READ_DEADLINE:?} after the \
                 ABR mode and bitrate-cap storm — loading is wedged"
            )
        })
        .unwrap_or_else(|error| panic!("read after the ABR storm failed: {error}"));
    assert!(
        bytes > 0,
        "the stream reported end-of-input rather than audio after the ABR storm"
    );
}
