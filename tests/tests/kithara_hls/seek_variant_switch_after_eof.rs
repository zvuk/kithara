#![forbid(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    hls_test_helpers::pin_abr_variant,
    rt_cancel, temp_dir,
};
use kithara_platform::{CancellationToken, time::Duration, tokio::task::spawn_blocking};
use tracing::info;

/// Seek after ABR variant switch at EOF must not deadlock.
///
/// Reproduces the production bug where `handle_midstream_switch()` unconditionally
/// drains all segment requests, discarding the on-demand request from the new seek epoch.
#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_hls=debug,kithara_stream=debug,kithara_decode=debug")
)]
async fn seek_after_variant_switch_at_eof_must_not_deadlock(
    temp_dir: TestTempDir,
    rt_cancel: CancellationToken,
) {
    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 3,
        segments_per_variant: 3,
        segment_size: 200_000,
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");

    let config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(rt_cancel)
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    spawn_blocking(move || {
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
        }
        info!("All variant 0 data read to EOF");

        let abr = stream.abr_handle().expect("HLS source exposes AbrHandle");
        pin_abr_variant(&abr, 1);
        info!("ABR variant switched 0 → 1");

        let seek_pos = 200_000u64;
        let pos = stream.seek(SeekFrom::Start(seek_pos)).unwrap();
        assert_eq!(pos, seek_pos);
        info!(seek_pos, "Seek applied");

        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "Read after seek + variant switch must return data");
        info!(n, "Read succeeded after seek + variant switch");
    })
    .await
    .unwrap();
}
