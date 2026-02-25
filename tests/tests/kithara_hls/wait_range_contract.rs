//! Contract tests for `wait_range` behavior through public `Stream<Hls>` API.
//!
//! These tests intentionally avoid touching HLS internals and validate
//! user-visible guarantees:
//! 1. Rapid seek burst never returns premature EOF.
//! 2. After seek burst, sequential tail read is contiguous and exact.

use std::io::{Read, Seek, SeekFrom};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_test_utils::{TestTempDir, Xorshift64};
use tokio_util::sync::CancellationToken;

use super::fixture::{HlsTestServer, HlsTestServerConfig};

const SEGMENT_SIZE: usize = 50_000;
const SEGMENT_COUNT: usize = 40;
const SEEK_ITERATIONS: usize = 800;
const PROBE_SIZE: usize = 64;
const TAIL_CHUNK_SIZE: usize = 32 * 1024;

#[kithara::test(tokio, browser, timeout(Duration::from_secs(60)))]
#[case::disk(false)]
#[case::ephemeral(true)]
async fn seek_burst_then_tail_read_stays_contiguous(#[case] ephemeral: bool) {
    let temp_dir = TestTempDir::new();
    let server = HlsTestServer::new(HlsTestServerConfig {
        segment_size: SEGMENT_SIZE,
        segments_per_variant: SEGMENT_COUNT,
        ..Default::default()
    })
    .await;
    let url = server.url("/master.m3u8").expect("url");

    let store = StoreOptions::new(temp_dir.path()).with_ephemeral(ephemeral);
    let config = HlsConfig::new(url)
        .with_store(store)
        .with_cancel(CancellationToken::new())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });
    let mut stream = Stream::<Hls>::new(config).await.expect("create stream");

    let total_bytes = server.total_bytes();
    assert!(
        total_bytes > PROBE_SIZE as u64 + 1,
        "fixture stream must be larger than probe"
    );

    let result = kithara_platform::spawn_blocking(move || {
        // Phase 1: dense seek burst with immediate probe reads.
        let mut rng = Xorshift64::new(0xA11C_EE55_D00D_BA5E);
        let max_seek = total_bytes - PROBE_SIZE as u64;
        let mut probe = [0u8; PROBE_SIZE];

        for _ in 0..SEEK_ITERATIONS {
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

        // Phase 2: sequential tail read must stay contiguous and exact.
        let tail_start = total_bytes / 3;
        let actual = stream
            .seek(SeekFrom::Start(tail_start))
            .expect("tail seek must succeed");
        assert_eq!(actual, tail_start);

        let mut tail_buf = vec![0u8; TAIL_CHUNK_SIZE];
        let mut offset = tail_start;
        let mut total_read = 0u64;

        loop {
            let n = stream
                .read(&mut tail_buf)
                .expect("tail read must not fail after seek burst");
            if n == 0 {
                break;
            }

            for (i, &byte) in tail_buf[..n].iter().enumerate() {
                let expected = server.expected_byte_at(0, offset + i as u64);
                assert_eq!(
                    byte,
                    expected,
                    "tail byte mismatch at absolute offset {}",
                    offset + i as u64
                );
            }

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
