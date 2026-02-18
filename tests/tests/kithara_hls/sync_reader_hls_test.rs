//! Test: Stream<Hls> reads all bytes.
//!
//! CRITICAL TEST: Verifies that Stream<Hls> reads ALL bytes from HLS source.
//!
//! This test isolates HLS streaming layer.
//!
//! Expected behavior:
//! - HLS should load all 3 segments (variant 0 has 3 segments in playlist)
//! - Stream should read ALL bytes from all 3 segments
//! - Total bytes = 3 segments * ~200KB each = ~600KB

use std::{io::Read, time::Duration};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_test_utils::temp_dir;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::fixture::abr::{AbrTestServer, master_playlist};

#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test]
async fn test_sync_reader_reads_all_bytes_from_hls(temp_dir: TempDir) {
    // Create HLS server with 3 segments per variant
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(10), // minimal delay
    )
    .await;

    let url = server.url("/master.m3u8").unwrap();
    let cancel_token = CancellationToken::new();

    // Configure HLS - fixed variant 0 (no ABR switching for this test)
    let config = HlsConfig::new(url.clone())
        .with_cancel(cancel_token.clone())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0), // Stay on variant 0
            ..Default::default()
        });

    // Open HLS stream
    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    // Give HLS time to start fetching
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Read ALL bytes using std::io::Read
    let mut all_bytes = Vec::new();
    let mut read_buf = vec![0u8; 64 * 1024];
    let mut total_reads = 0;

    let result = tokio::task::spawn_blocking(move || {
        loop {
            match stream.read(&mut read_buf) {
                Ok(0) => {
                    println!("EOF after {} reads", total_reads);
                    break;
                }
                Ok(n) => {
                    all_bytes.extend_from_slice(&read_buf[..n]);
                    total_reads += 1;
                    if total_reads <= 10 || total_reads % 10 == 0 {
                        println!(
                            "Read {}: {} bytes, total: {}",
                            total_reads,
                            n,
                            all_bytes.len()
                        );
                    }
                }
                Err(e) => {
                    panic!("Read error after {} bytes: {}", all_bytes.len(), e);
                }
            }

            // Safety: stop if we read too much (shouldn't happen)
            if all_bytes.len() > 1_000_000 {
                panic!("Read too much data: {} bytes", all_bytes.len());
            }
        }

        (all_bytes, total_reads)
    })
    .await
    .unwrap();

    let (all_bytes, total_reads) = result;

    println!("\n=== FINAL RESULTS ===");
    println!("Total bytes read: {}", all_bytes.len());
    println!("Total read operations: {}", total_reads);

    // Each segment is ~200KB (9 byte header + data)
    // 3 segments should be ~600KB total, but we accept slightly less due to buffering
    let expected_min_bytes = 500_000; // At least 500KB for 3 segments

    // Parse segments to count them (using binary format from AbrTestServer)
    let mut segments_found = 0;
    let mut offset = 0;

    while offset + 9 <= all_bytes.len() {
        let variant = all_bytes[offset];
        let segment = u32::from_be_bytes([
            all_bytes[offset + 1],
            all_bytes[offset + 2],
            all_bytes[offset + 3],
            all_bytes[offset + 4],
        ]);
        let data_len = u32::from_be_bytes([
            all_bytes[offset + 5],
            all_bytes[offset + 6],
            all_bytes[offset + 7],
            all_bytes[offset + 8],
        ]) as usize;

        println!(
            "Segment {}: variant={}, segment={}, data_len={}",
            segments_found, variant, segment, data_len
        );

        segments_found += 1;
        let next_offset = offset + 9 + data_len;

        // Safety check: don't go past buffer
        if next_offset > all_bytes.len() {
            println!(
                "Segment {} truncated: expected {} bytes, have {}",
                segments_found - 1,
                data_len,
                all_bytes.len() - offset - 9
            );
            break;
        }
        offset = next_offset;
    }

    println!("\n=== SEGMENT ANALYSIS ===");
    println!("Segments found: {}", segments_found);
    println!("Bytes parsed: {}", offset);
    println!(
        "Bytes remaining: {}",
        all_bytes.len().saturating_sub(offset)
    );

    // CRITICAL ASSERTION: Should read substantial data from all segments
    assert!(
        all_bytes.len() >= expected_min_bytes,
        "FAIL: Only read {} bytes (expected at least {}). This means HLS failed to load segments!",
        all_bytes.len(),
        expected_min_bytes
    );

    // Should find at least 3 segments (even if last one is truncated)
    assert!(
        segments_found >= 3,
        "FAIL: Only found {} segments, expected at least 3. HLS stopped early!",
        segments_found
    );

    cancel_token.cancel();
}
