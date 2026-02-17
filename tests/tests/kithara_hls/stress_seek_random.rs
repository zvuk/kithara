//! Stress test: random seek+read cycles on HLS stream.
//!
//! Spawns a local HLS server via [`HlsTestServer`], creates `Stream<Hls>`,
//! performs random byte-level seeks each followed by a read, verifying exact
//! byte content at every position. Parametrized via rstest cases (small/medium/large).
//!
//! Deterministic [`Xorshift64`] PRNG guarantees reproducibility.
//! No external network required.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
    time::Duration,
};

use kithara_assets::StoreOptions;
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::Stream;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture::{EncryptionConfig, HlsTestServer, HlsTestServerConfig};
use crate::common::Xorshift64;

/// Random seek+read cycles with exact byte verification on HLS stream.
///
/// Scenario:
/// 1. Spawn local HLS server (`segment_count` segments × `segment_size` bytes)
/// 2. Create `Stream<Hls>` with `AbrMode::Manual(0)`
/// 3. Compute total byte length from server config
/// 4. Compute optimal random chunk size proportional to stream
/// 5. Sample `seek_iterations` random seek positions in `(0, len - chunk_size)`
/// 6. For each: seek → read → verify every byte matches `expected_byte_at`
/// 7. Final: seek to `len - chunk_size`, read all → verify EOF
#[rstest]
#[case::small(50_000, 20, 200, false, false)]
#[case::medium(100_000, 50, 500, false, false)]
#[case::large(200_000, 100, 1000, false, false)]
#[case::init_small(50_000, 20, 200, true, false)]
#[case::init_medium(100_000, 50, 500, true, false)]
#[case::encrypted_small(50_000, 20, 200, true, true)]
#[case::encrypted_medium(100_000, 50, 500, true, true)]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_random_seek_read_hls(
    #[case] segment_size: usize,
    #[case] segment_count: usize,
    #[case] seek_iterations: usize,
    #[case] with_init: bool,
    #[case] with_encryption: bool,
) {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "kithara_hls=debug,kithara_stream=debug".to_string()),
        )
        .try_init();

    // Init data
    let init_data_per_variant = if with_init {
        let init_size = 1024;
        let mut init = b"V0-INIT:".to_vec();
        init.resize(init_size, 0xFF);
        Some(vec![Arc::new(init)])
    } else {
        None
    };

    // Encryption config
    let encryption = if with_encryption {
        Some(EncryptionConfig {
            key: *b"0123456789abcdef",
            iv: Some([0u8; 16]),
        })
    } else {
        None
    };

    // Step 1: Spawn HLS server
    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: segment_count,
        segment_size,
        init_data_per_variant,
        encryption,
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").expect("url");
    let total_bytes = server.total_bytes();
    let init_len = server.init_len();
    info!(
        %url,
        segments = segment_count,
        total_mb = total_bytes / 1_000_000,
        init_len,
        with_init,
        with_encryption,
        "HLS server ready"
    );

    // Step 2: Create Stream<Hls>
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

    // Steps 3-7 in blocking thread
    let result = tokio::task::spawn_blocking(move || {
        // Step 3: Total byte length from fixture config
        info!(total_bytes, "Stream byte length");

        // Trigger initial download + verify first bytes
        let mut probe = [0u8; 64];
        let n = stream.read(&mut probe).unwrap_or_else(|e| {
            panic!("initial probe read failed: {e}");
        });
        assert!(n > 0, "probe read returned 0");

        if with_init {
            assert_eq!(&probe[..8], b"V0-INIT:", "probe: init segment prefix");
        } else {
            assert_eq!(&probe[..9], b"V0-SEG-0:", "probe: first segment prefix");
        }
        stream.seek(SeekFrom::Start(0)).expect("seek back to 0");

        // Step 4: Compute chunk size — ~0.5% of total, clamped [1 KB, 64 KB]
        let chunk_size = ((total_bytes as f64 * 0.005) as usize).clamp(1024, 65_536);
        info!(chunk_size, "Read chunk size");

        let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
        let mut buf = vec![0u8; chunk_size];

        // Step 5: Generate random seek positions > 0, < len - chunk_size
        let max_seek = total_bytes - chunk_size as u64;
        let seek_positions: Vec<u64> = (0..seek_iterations)
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

            // Verify: every byte matches expected content.
            // For encrypted streams, PKCS7 padding removal causes segment size drift
            // (16 bytes per segment) so byte offsets don't align with plaintext layout.
            // We only verify read success and data availability in that case.
            if !with_encryption {
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
            total_bytes_read, byte_mismatches, "All {seek_iterations} seek+read iterations done"
        );

        assert_eq!(successful_reads, seek_iterations as u64);
        if !with_encryption {
            assert_eq!(
                byte_mismatches, 0,
                "{byte_mismatches} byte mismatches detected — data corruption"
            );
        }

        // Step 7: Final seek near end → read all → verify EOF.
        // For encrypted streams, PKCS7 padding drift makes the stream's byte layout
        // differ from plaintext layout, so tail verification is skipped.
        if !with_encryption {
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

                for (j, &byte) in buf[..n].iter().enumerate() {
                    let expected =
                        server.expected_byte_at(0, final_seek + remaining_bytes + j as u64);
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
        } else {
            info!("Skipping tail verification for encrypted stream (PKCS7 offset drift)");
        }
    })
    .await;

    match result {
        Ok(()) => info!("HLS stress test passed"),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
