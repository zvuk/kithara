#![forbid(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};

use kithara::platform::{CancelToken, time::Duration, tokio::task::spawn_blocking};
use kithara_integration_tests::{
    TestTempDir, hls_fixture::HlsStreamBuilder, hls_server::TestServer, rt_cancel, temp_dir,
};

#[derive(Clone, Copy)]
enum SeekScenario {
    SegmentStart(u64, &'static [u8]),
    Current,
    Multiple,
    ReadAllThenBack,
    AcrossAll,
}

fn assert_read(stream: &mut impl Read, expected: &[u8]) {
    let mut actual = vec![0; expected.len()];
    stream.read_exact(&mut actual).unwrap();
    assert_eq!(actual, expected);
}

fn run_seek_scenario(mut stream: impl Read + Seek, scenario: SeekScenario) {
    match scenario {
        SeekScenario::SegmentStart(position, expected) => {
            assert_eq!(stream.seek(SeekFrom::Start(position)).unwrap(), position);
            assert_read(&mut stream, expected);
        }
        SeekScenario::Current => {
            let mut ignored = [0; 10];
            stream.read_exact(&mut ignored).unwrap();
            assert_eq!(stream.seek(SeekFrom::Current(19)).unwrap(), 29);
            assert_read(&mut stream, &[0xFF; 6]);
        }
        SeekScenario::Multiple => {
            assert_read(&mut stream, b"V0-SEG-0:");
            stream.seek(SeekFrom::Start(0)).unwrap();
            assert_read(&mut stream, b"V0-SEG-0:");
            stream.seek(SeekFrom::Start(100)).unwrap();
            assert_read(&mut stream, &[0xFF; 6]);
        }
        SeekScenario::ReadAllThenBack => {
            let mut all_data = Vec::new();
            stream.read_to_end(&mut all_data).unwrap();
            assert!(
                all_data.len() > 500_000,
                "Should read substantial data, got {} bytes",
                all_data.len()
            );
            assert!(all_data.starts_with(b"V0-SEG-0:"));
            stream.seek(SeekFrom::Start(0)).unwrap();
            assert_read(&mut stream, b"V0-SEG-0:");
        }
        SeekScenario::AcrossAll => {
            assert_read(&mut stream, b"V0-SEG-0:");
            stream.seek(SeekFrom::Start(100)).unwrap();
            assert_read(&mut stream, &[0xFF; 10]);
            stream.seek(SeekFrom::Start(0)).unwrap();
            assert_read(&mut stream, b"V0-SEG-0:");
        }
    }
}

#[kithara::test(tokio, native, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
#[case::segment_0(SeekScenario::SegmentStart(0, b"V0-SEG-0:"))]
#[case::segment_1(SeekScenario::SegmentStart(200_000, b"V0-SEG-1:"))]
#[case::segment_2(SeekScenario::SegmentStart(400_000, b"V0-SEG-2:"))]
#[case::current(SeekScenario::Current)]
#[case::multiple(SeekScenario::Multiple)]
#[case::read_all_then_back(SeekScenario::ReadAllThenBack)]
#[case::across_all(SeekScenario::AcrossAll)]
async fn hls_stream_seek(
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
    #[case] scenario: SeekScenario,
) {
    let server = TestServer::new().await;
    let stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

    spawn_blocking(move || run_seek_scenario(stream, scenario))
        .await
        .unwrap();
}

#[kithara::test(tokio, native, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
async fn hls_with_manual_abr_uses_fixed_variant(temp_dir: TestTempDir, rt_cancel: CancelToken) {
    let server = TestServer::new().await;
    let mut stream = HlsStreamBuilder::new()
        .variant(1)
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

    spawn_blocking(move || {
        assert_read(&mut stream, b"V1-SEG-0:");
    })
    .await
    .unwrap();
}

/// Test that demonstrates ABR switch + seek behavior with manual variant selection.
///
/// This test shows that different variants produce different data at the same positions,
/// which is the foundation for ABR switch + seek correctness.
#[kithara::test(tokio, native, timeout(Duration::from_secs(15)), hang_timeout_secs(1))]
async fn hls_seek_different_variants_return_different_data(
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
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
        assert_read(&mut stream_v0, b"V0-SEG-0:");
        assert_read(&mut stream_v1, b"V1-SEG-0:");

        stream_v0.seek(SeekFrom::Start(0)).unwrap();
        stream_v1.seek(SeekFrom::Start(0)).unwrap();

        assert_read(&mut stream_v0, b"V0-SEG-0:");
        assert_read(&mut stream_v1, b"V1-SEG-0:");
    })
    .await
    .unwrap();
}
