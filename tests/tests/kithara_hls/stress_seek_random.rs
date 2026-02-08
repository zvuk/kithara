//! Stress test: 1000 random seek+read cycles on HLS stream (20 MB).
//!
//! Spawns a local HLS server with 100 segments × 200 KB = 20 MB via
//! [`HlsTestServer`], creates `Stream<Hls>`, performs 1000 random byte-level
//! seeks each followed by a read, verifying exact byte content at every position.
//!
//! Deterministic [`Xorshift64`] PRNG guarantees reproducibility.
//! No external network required.

use std::{
    io::{Read, Seek, SeekFrom},
    time::Duration,
};

use kithara_assets::StoreOptions;
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::Stream;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture::{HlsTestServer, HlsTestServerConfig};
use crate::common::Xorshift64;

const SEGMENT_SIZE: usize = 200_000;
const SEGMENT_COUNT: usize = 100;
const SEEK_ITERATIONS: usize = 1000;

/// 1000 random seek+read cycles with exact byte verification on 20 MB HLS stream.
///
/// Scenario:
/// 1. Spawn local HLS server (100 segments × 200 KB = 20 MB)
/// 2. Create `Stream<Hls>` with `AbrMode::Manual(0)`
/// 3. Compute total byte length from server config
/// 4. Compute optimal random chunk size proportional to stream
/// 5. Sample 1000 random seek positions in `(0, len - chunk_size)`
/// 6. For each: seek → read → verify every byte matches `expected_byte_at`
/// 7. Final: seek to `len - chunk_size`, read all → verify EOF
#[rstest]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_random_seek_read_hls() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "kithara_hls=debug,kithara_stream=debug".to_string()),
        )
        .try_init();

    // --- Step 1: Spawn HLS server ---
    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").expect("url");
    let total_bytes = server.total_bytes();
    info!(
        %url,
        segments = SEGMENT_COUNT,
        total_mb = total_bytes / 1_000_000,
        "HLS server ready"
    );

    // --- Step 2: Create Stream<Hls> ---
    let temp_dir = TempDir::new().expect("temp dir");
    let cancel = CancellationToken::new();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.expect("create HLS stream");

    // --- Steps 3-7 in blocking thread ---
    let result = tokio::task::spawn_blocking(move || {
        // Step 3: Total byte length from fixture config
        info!(total_bytes, "Stream byte length");

        // Trigger initial download + verify first segment prefix
        let mut probe = [0u8; 64];
        let n = stream.read(&mut probe).unwrap_or_else(|e| {
            panic!("initial probe read failed: {e}");
        });
        assert!(n > 0, "probe read returned 0");
        assert_eq!(&probe[..9], b"V0-SEG-0:", "probe: first segment prefix");
        stream.seek(SeekFrom::Start(0)).expect("seek back to 0");

        // Step 4: Compute chunk size — ~0.5% of total, clamped [1 KB, 64 KB]
        let chunk_size = ((total_bytes as f64 * 0.005) as usize).clamp(1024, 65_536);
        info!(chunk_size, "Read chunk size");

        let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
        let mut buf = vec![0u8; chunk_size];

        // Step 5: Generate 1000 random seek positions > 0, < len - chunk_size
        let max_seek = total_bytes - chunk_size as u64;
        let seek_positions: Vec<u64> = (0..SEEK_ITERATIONS)
            .map(|_| rng.range_u64(1, max_seek))
            .collect();

        info!(
            count = seek_positions.len(),
            max_seek, "Generated seek positions"
        );

        // Step 6: Iterate seek + read + verify
        let mut successful_reads = 0u64;
        let mut total_bytes_read = 0u64;
        let mut byte_mismatches = 0u64;

        for (i, &pos) in seek_positions.iter().enumerate() {
            // Seek
            let actual_pos = stream.seek(SeekFrom::Start(pos)).unwrap_or_else(|e| {
                panic!("seek #{i} to byte {pos} failed: {e}");
            });
            assert_eq!(actual_pos, pos, "seek #{i} position mismatch");

            // Read
            let n = stream.read(&mut buf).unwrap_or_else(|e| {
                panic!("read #{i} after seek to {pos} failed: {e}");
            });
            assert!(
                n > 0,
                "read returned 0 after seek #{i} to byte {pos} (total_len={total_bytes})",
            );

            // Verify: every byte matches expected content
            for (j, &byte) in buf[..n].iter().enumerate() {
                let expected = server.expected_byte_at(0, pos + j as u64);
                if byte != expected {
                    byte_mismatches += 1;
                    if byte_mismatches <= 5 {
                        info!(
                            iteration = i,
                            offset = pos + j as u64,
                            expected,
                            actual = byte,
                            "Byte mismatch"
                        );
                    }
                }
            }

            successful_reads += 1;
            total_bytes_read += n as u64;

            if (i + 1) % 200 == 0 {
                info!(
                    iteration = i + 1,
                    successful_reads, total_bytes_read, byte_mismatches, "Progress"
                );
            }
        }

        info!(
            successful_reads,
            total_bytes_read, byte_mismatches, "All {SEEK_ITERATIONS} seek+read iterations done"
        );

        assert_eq!(successful_reads, SEEK_ITERATIONS as u64);
        assert_eq!(
            byte_mismatches, 0,
            "{byte_mismatches} byte mismatches detected — data corruption"
        );

        // Step 7: Final seek near end → read all → verify EOF
        let final_seek = total_bytes - chunk_size as u64;
        info!(final_seek, "Final seek near end");

        stream
            .seek(SeekFrom::Start(final_seek))
            .unwrap_or_else(|e| {
                panic!("final seek to {final_seek} failed: {e}");
            });

        let mut remaining_bytes = 0u64;
        loop {
            let n = stream.read(&mut buf).unwrap_or_else(|e| {
                panic!("final tail read failed: {e}");
            });
            if n == 0 {
                break;
            }

            // Verify tail bytes
            for (j, &byte) in buf[..n].iter().enumerate() {
                let expected = server.expected_byte_at(0, final_seek + remaining_bytes + j as u64);
                assert_eq!(
                    byte,
                    expected,
                    "tail byte mismatch at offset {}",
                    final_seek + remaining_bytes + j as u64
                );
            }
            remaining_bytes += n as u64;
        }

        let expected_remaining = total_bytes - final_seek;
        assert_eq!(
            remaining_bytes, expected_remaining,
            "tail read: expected {expected_remaining} bytes, got {remaining_bytes}"
        );

        info!(remaining_bytes, "Final read done — EOF confirmed");
    })
    .await;

    match result {
        Ok(()) => info!("HLS stress test passed"),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
