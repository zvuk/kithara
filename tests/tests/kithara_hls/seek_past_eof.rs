#![forbid(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir, cancel_token,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    temp_dir,
};
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use tokio_util::sync::CancellationToken;

use crate::common::test_defaults::Consts as Shared;

struct Consts;
impl Consts {
    /// Actual segment body size returned by GET.
    const ACTUAL_SEGMENT_SIZE: usize = Shared::SEGMENT_SIZE;

    /// Content-Length reported by HEAD (smaller, simulating compressed size).
    /// ~800 bytes less per segment — matches real-world gzip/brotli overhead.
    const HEAD_REPORTED_SIZE: usize = 199_200;

    const NUM_SEGMENTS: usize = 3;

    /// HEAD-based total across all segments (no init segments in this test).
    const HEAD_TOTAL: u64 = (Self::HEAD_REPORTED_SIZE * Self::NUM_SEGMENTS) as u64;

    /// Actual total bytes that will be downloaded and cached.
    const ACTUAL_TOTAL: u64 = (Self::ACTUAL_SEGMENT_SIZE * Self::NUM_SEGMENTS) as u64;
}

/// Seek to a position between HEAD-reported total and actual total must succeed.
///
/// Reproduces the production "seek past EOF" bug:
/// - HEAD total: `597_600` (3 x `199_200`)
/// - Actual total: `600_000` (3 x `200_000`)
/// - Seek to `598_000` is valid but fails if `expected_total_length` = HEAD total.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(15)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_hls=debug,kithara_stream=debug,kithara_decode=debug")
)]
async fn seek_beyond_head_total_within_actual_total(
    temp_dir: TestTempDir,
    cancel_token: CancellationToken,
) {
    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: Consts::NUM_SEGMENTS,
        segment_size: Consts::ACTUAL_SEGMENT_SIZE,
        head_reported_segment_size: Some(Consts::HEAD_REPORTED_SIZE),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");

    let config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel_token)
        .initial_abr_mode(AbrMode::Manual(0))
        .build();

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    spawn_blocking(move || {
        let mut all_data = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
        }

        assert!(
            all_data.len() as u64 > Consts::HEAD_TOTAL,
            "Read {} bytes, expected more than HEAD total {}",
            all_data.len(),
            Consts::HEAD_TOTAL,
        );

        let seek_target = Consts::HEAD_TOTAL + 1;
        let result = stream.seek(SeekFrom::Start(seek_target));

        assert!(
            result.is_ok(),
            "Seek to {} should succeed (HEAD total={}, actual total={}): {:?}",
            seek_target,
            Consts::HEAD_TOTAL,
            Consts::ACTUAL_TOTAL,
            result.err(),
        );

        let mut buf = [0u8; 16];
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "Should read data after seek to {}", seek_target);

        assert_eq!(&buf[..n], &vec![0xFF; n][..], "Expected padding bytes");
    })
    .await
    .unwrap();
}
