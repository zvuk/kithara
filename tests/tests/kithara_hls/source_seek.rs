#![forbid(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};

use kithara_integration_tests::{
    TestTempDir, hls_fixture::HlsStreamBuilder, hls_server::TestServer, rt_cancel, temp_dir,
};
use kithara_platform::{CancellationToken, time::Duration, tokio::task::spawn_blocking};
use tracing::info;

/// Segment size in bytes (test fixture pads to 200KB).
const SEGMENT_SIZE: u64 = 200_000;

#[kithara::test(
    flash(false),
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(0, b"V0-SEG-0:")]
#[case(200_000, b"V0-SEG-1:")]
#[case(400_000, b"V0-SEG-2:")]
async fn hls_stream_seek_to_segment_start(
    temp_dir: TestTempDir,
    rt_cancel: CancellationToken,
    #[case] seek_pos: u64,
    #[case] expected_prefix: &[u8],
) {
    let server = TestServer::new().await;
    let mut stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

    let expected_len = expected_prefix.len();
    let expected_vec = expected_prefix.to_vec();

    let result = spawn_blocking(move || {
        let pos = stream.seek(SeekFrom::Start(seek_pos)).unwrap();
        assert_eq!(pos, seek_pos);

        let mut buf = vec![0u8; expected_len];
        let n = stream.read(&mut buf).unwrap();
        (n, buf)
    })
    .await
    .unwrap();

    assert_eq!(result.0, expected_len);
    assert_eq!(&result.1[..result.0], &expected_vec[..]);
}

#[kithara::test(
    flash(false),
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn hls_stream_seek_current(temp_dir: TestTempDir, rt_cancel: CancellationToken) {
    let server = TestServer::new().await;
    let mut stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

    spawn_blocking(move || {
        let mut buf = [0u8; 10];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 10);

        let pos = stream.seek(SeekFrom::Current(19)).unwrap();
        assert_eq!(pos, 29);

        let mut buf = [0u8; 6];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf, &[0xFF; 6]);
    })
    .await
    .unwrap();
}

#[kithara::test(
    flash(false),
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn hls_stream_multiple_seeks(temp_dir: TestTempDir, rt_cancel: CancellationToken) {
    let server = TestServer::new().await;
    let mut stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

    spawn_blocking(move || {
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"V0-SEG-0:");

        stream.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"V0-SEG-0:", "After seek to 0, should read segment 0");

        stream.seek(SeekFrom::Start(100)).unwrap();
        let mut buf = [0u8; 6];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf, &[0xFF; 6], "Position 100 should be padding bytes");
    })
    .await
    .unwrap();
}

#[kithara::test(
    flash(false),
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn hls_stream_read_all_then_seek_back(temp_dir: TestTempDir, rt_cancel: CancellationToken) {
    let server = TestServer::new().await;
    let mut stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

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
            all_data.len() > 500_000,
            "Should read substantial data, got {} bytes",
            all_data.len()
        );

        assert!(
            all_data.starts_with(b"V0-SEG-0:"),
            "Data should start with V0-SEG-0:"
        );

        stream.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(
            &buf, b"V0-SEG-0:",
            "After seek to 0, should read segment 0 prefix"
        );
    })
    .await
    .unwrap();
}

#[kithara::test(
    flash(false),
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn hls_with_manual_abr_uses_fixed_variant(
    temp_dir: TestTempDir,
    rt_cancel: CancellationToken,
) {
    let server = TestServer::new().await;
    let mut stream = HlsStreamBuilder::new()
        .variant(1)
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

    spawn_blocking(move || {
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);

        assert_eq!(&buf, b"V1-SEG-0:");
    })
    .await
    .unwrap();
}

#[kithara::test(
    flash(false),
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn hls_seek_across_all_segments_with_fixed_abr(
    temp_dir: TestTempDir,
    rt_cancel: CancellationToken,
) {
    let server = TestServer::new().await;

    info!("Testing seek across all segments with fixed ABR");

    let mut stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

    spawn_blocking(move || {
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"V0-SEG-0:", "Start should be segment 0 prefix");

        stream.seek(SeekFrom::Start(100)).unwrap();
        let mut buf = [0u8; 10];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 10);
        assert_eq!(&buf, &[0xFF; 10], "Position 100 should be padding");

        stream.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(
            &buf, b"V0-SEG-0:",
            "After seek back, should still be segment 0"
        );
    })
    .await
    .unwrap();
}

/// Test that demonstrates ABR switch + seek behavior with manual variant selection.
///
/// This test shows that different variants produce different data at the same positions,
/// which is the foundation for ABR switch + seek correctness.
#[kithara::test(
    flash(false),
    tokio,
    native,
    timeout(Duration::from_secs(15)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn hls_seek_different_variants_return_different_data(
    temp_dir: TestTempDir,
    rt_cancel: CancellationToken,
) {
    let server = TestServer::new().await;

    let mut stream_v0 = HlsStreamBuilder::new()
        .variant(0)
        .store_subdir("v0")
        .build(&server, temp_dir.path(), rt_cancel.clone())
        .await;

    let mut stream_v1 = HlsStreamBuilder::new()
        .variant(1)
        .store_subdir("v1")
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

    spawn_blocking(move || {
        let mut buf_v0 = [0u8; 9];
        let mut buf_v1 = [0u8; 9];
        let n0 = stream_v0.read(&mut buf_v0).unwrap();
        let n1 = stream_v1.read(&mut buf_v1).unwrap();
        assert_eq!(n0, 9);
        assert_eq!(n1, 9);

        assert_eq!(&buf_v0, b"V0-SEG-0:");
        assert_eq!(&buf_v1, b"V1-SEG-0:");
        assert_ne!(
            &buf_v0, &buf_v1,
            "Different variants should have different data"
        );

        stream_v0.seek(SeekFrom::Start(0)).unwrap();
        stream_v1.seek(SeekFrom::Start(0)).unwrap();

        let mut after_v0 = [0u8; 9];
        let mut after_v1 = [0u8; 9];
        let n0 = stream_v0.read(&mut after_v0).unwrap();
        let n1 = stream_v1.read(&mut after_v1).unwrap();
        assert_eq!(n0, 9);
        assert_eq!(n1, 9);

        assert_eq!(&after_v0, b"V0-SEG-0:", "Variant 0 data after seek");
        assert_eq!(&after_v1, b"V1-SEG-0:", "Variant 1 data after seek");
    })
    .await
    .unwrap();
}
