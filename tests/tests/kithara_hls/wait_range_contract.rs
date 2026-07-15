use std::{
    io::{Read, Seek, SeekFrom},
    num::NonZeroUsize,
};

use kithara::{
    assets::{AssetStoreBuilder, StorageBackend},
    hls::{AbrMode, Hls, HlsConfig},
    platform::{CancelToken, time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir, Xorshift64,
    hls_server::{HlsTestServer, HlsTestServerConfig},
};

struct Consts;
impl Consts {
    const SEGMENT_SIZE: usize = 50_000;
    const SEGMENT_COUNT: usize = 40;
    const SEEK_ITERATIONS: usize = 800;
    const PROBE_SIZE: usize = 64;
    const TAIL_CHUNK_SIZE: usize = 32 * 1024;
}

#[kithara::test(
    tokio,
    serial,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::ephemeral(true)]
#[cfg(not(target_arch = "wasm32"))]
#[case::disk(false)]
async fn seek_burst_then_tail_read_stays_contiguous(#[case] ephemeral: bool) {
    let temp_dir = TestTempDir::new();
    let server = HlsTestServer::new(HlsTestServerConfig {
        segment_size: Consts::SEGMENT_SIZE,
        segments_per_variant: Consts::SEGMENT_COUNT,
        ..Default::default()
    })
    .await;
    let url = server.url("/master.m3u8");

    let backend = if ephemeral {
        StorageBackend::Memory
    } else {
        StorageBackend::Disk {
            root: temp_dir.path().into(),
        }
    };
    let store = AssetStoreBuilder::default()
        .backend(backend)
        .cache_capacity(NonZeroUsize::new(256).unwrap())
        .build();
    let config = HlsConfig::for_url(url)
        .store(store)
        .cancel(CancelToken::never())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let mut stream = Stream::<Hls>::new(config).await.expect("create stream");

    let total_bytes = server.total_bytes();
    assert!(
        total_bytes > Consts::PROBE_SIZE as u64 + 1,
        "fixture stream must be larger than probe"
    );

    let result = spawn_blocking(move || {
        let mut rng = Xorshift64::new(0xA11C_EE55_D00D_BA5E);
        let max_seek = total_bytes - Consts::PROBE_SIZE as u64;
        let mut probe = [0u8; Consts::PROBE_SIZE];

        for _ in 0..Consts::SEEK_ITERATIONS {
            let seek_pos = rng.range_u64(1, max_seek);
            let actual = stream
                .seek(SeekFrom::Start(seek_pos))
                .expect("seek in burst must succeed");
            assert_eq!(actual, seek_pos, "seek returned unexpected position");

            let n = stream.read(&mut probe).expect("probe read must succeed");
            assert!(n > 0, "probe read returned EOF during seek burst");
            assert_eq!(
                probe[0],
                server.expected_byte_at(0, seek_pos),
                "probe first byte must match fixture data",
            );
        }

        let tail_start = total_bytes / 3;
        let actual = stream
            .seek(SeekFrom::Start(tail_start))
            .expect("tail seek must succeed");
        assert_eq!(actual, tail_start);

        let mut tail_buf = vec![0u8; Consts::TAIL_CHUNK_SIZE];
        let mut offset = tail_start;
        let mut total_read = 0u64;

        loop {
            let n = stream
                .read(&mut tail_buf)
                .expect("tail read must not fail after seek burst");
            if n == 0 {
                break;
            }

            server.for_each_expected_byte_mismatch(
                0,
                offset,
                &tail_buf[..n],
                |i, expected, byte| {
                    assert_eq!(
                        byte,
                        expected,
                        "tail byte mismatch at absolute offset {}",
                        offset + i as u64
                    );
                },
            );

            offset += n as u64;
            total_read += n as u64;
        }

        let expected_tail = total_bytes - tail_start;
        assert_eq!(
            total_read, expected_tail,
            "tail read size mismatch after seek burst"
        );
    })
    .await;

    match result {
        Ok(()) => {}
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}

/// Ephemeral stream with small LRU cache must read the entire stream
/// without deadlock or hang.
///
/// capacity=2 is the minimum for fMP4 segments with init+media resources.
/// Without `PlanOutcome::Idle` + backfill guard + TOCTOU Retry:
/// - downloader hot-spins on empty Batch → hang detector fires
/// - or backfill rewind re-downloads evicted segments infinitely
///
/// This test is RED before steps 4-6 fixes.
#[kithara::test(
    tokio,
    serial,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[cfg(not(target_arch = "wasm32"))]
async fn ephemeral_small_cache_reads_entire_stream() {
    let server = HlsTestServer::new(HlsTestServerConfig {
        segment_size: 20_000,
        segments_per_variant: 10,
        ..Default::default()
    })
    .await;
    let url = server.url("/master.m3u8");
    let total_bytes = server.total_bytes();

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(5).expect("5 > 0"))
        .build();
    let config = HlsConfig::for_url(url)
        .store(store)
        .cancel(CancelToken::never())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let mut stream = Stream::<Hls>::new(config).await.expect("create stream");

    let result = spawn_blocking(move || {
        let mut buf = vec![0u8; 8192];
        let mut total_read = 0u64;

        loop {
            let n = stream
                .read(&mut buf)
                .expect("read must not fail with small cache");
            if n == 0 {
                break;
            }
            total_read += n as u64;
        }

        assert_eq!(
            total_read, total_bytes,
            "must read entire stream with small ephemeral cache"
        );
    })
    .await;

    match result {
        Ok(()) => {}
        Err(e) => panic!("spawn_blocking failed (likely HangDetector panic): {e}"),
    }
}
