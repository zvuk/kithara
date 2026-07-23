#![forbid(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};

use kithara::{
    hls::{AbrMode, Hls, HlsConfig},
    platform::{CancelToken, time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    hls_test_helpers::pin_abr_variant,
    rt_cancel, temp_dir,
};
use tracing::info;

/// Seek after ABR variant switch at EOF must not deadlock.
///
/// Guards the switch-commit/seek race: `commit_variant_switch` must honor the
/// pending seek target (`seek_obs`) so the new variant's rebuilt plan includes
/// the segment the parked reader waits on (historically the midstream-switch
/// drain discarded the new seek epoch's on-demand request).
#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_hls=debug,kithara_stream=debug,kithara_decode=debug")
)]
async fn seek_after_variant_switch_at_eof_must_not_deadlock(
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
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
        .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
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

        // Raw byte consumer ack of the variant fence the structured-container
        // switch raised: in the audio path the decode FSM clears it via decoder
        // recreate; a byte-only `Stream` consumer has no FSM, so it acks the
        // fence directly and re-reads the new variant. If the switch-commit
        // dropped the seek-target segment (the deadlock this test guards), the
        // post-ack read finds no data and the 1s watchdog fires.
        let n = loop {
            match stream.read(&mut buf) {
                Ok(n) => break n,
                Err(e) if e.to_string().contains("variant change") => {
                    stream.clear_variant_fence();
                }
                Err(e) => panic!("read after seek + variant switch failed: {e}"),
            }
        };
        assert!(n > 0, "Read after seek + variant switch must return data");
        info!(n, "Read succeeded after seek + variant switch");
    })
    .await
    .unwrap();
}
