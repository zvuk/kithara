//! Test 8: SyncReader + StreamSource<Hls> without decoder.
//!
//! CRITICAL TEST: Verifies that SyncReader reads ALL bytes from HLS source.
//!
//! This test isolates HLS + SyncReader to determine if the problem
//! (only 2 segments loaded) is in HLS/SyncReader or in MockDecoder/Pipeline.
//!
//! Expected behavior:
//! - HLS should load all 3 segments (variant 0 has 3 segments in playlist)
//! - SyncReader should read ALL bytes from all 3 segments
//! - Total bytes = 3 segments * ~200KB each = ~600KB
//!
//! If this test FAILS (only reads 2 segments):
//! → Problem is in HLS or SyncReader, NOT in MockDecoder/Pipeline
//! → Need to debug HLS FetchManager or SyncReader EOF detection
//!
//! If this test PASSES (reads all 3 segments):
//! → Problem is in MockDecoder or Pipeline integration
//! → HLS itself works correctly

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use kithara_hls::{AbrMode, AbrOptions, Hls, HlsParams};
use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
use tokio_util::sync::CancellationToken;

use super::fixture::abr::{AbrTestServer, master_playlist};

#[tokio::test]
async fn test_sync_reader_reads_all_bytes_from_hls() {
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
    let hls_params = HlsParams::default()
        .with_cancel(cancel_token.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0), // Stay on variant 0
            ..Default::default()
        });

    // Open HLS source
    let source = StreamSource::<Hls>::open(url.clone(), hls_params)
        .await
        .unwrap();
    let source_arc = Arc::new(source);

    // Create SyncReader with large prefetch buffer
    let sync_params = SyncReaderParams {
        chunk_size: 64 * 1024,
        prefetch_chunks: 16, // 1MB buffer
    };
    let mut sync_reader = SyncReader::new(source_arc.clone(), sync_params);

    // Give HLS time to start fetching
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Read ALL bytes using std::io::Read
    let mut all_bytes = Vec::new();
    let mut read_buf = vec![0u8; 64 * 1024];
    let mut total_reads = 0;

    loop {
        match sync_reader.read(&mut read_buf) {
            Ok(0) => {
                println!("EOF after {} reads", total_reads);
                break;
            }
            Ok(n) => {
                all_bytes.extend_from_slice(&read_buf[..n]);
                total_reads += 1;
                if total_reads <= 10 || total_reads % 10 == 0 {
                    println!("Read {}: {} bytes, total: {}", total_reads, n, all_bytes.len());
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

    println!("\n=== FINAL RESULTS ===");
    println!("Total bytes read: {}", all_bytes.len());
    println!("Total read operations: {}", total_reads);

    // Each segment is ~200KB (9 byte header + data)
    // 3 segments should be ~600KB total
    let expected_min_bytes = 3 * 200_000; // At least 3 segments

    // Parse segments to count them
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
        offset += 9 + data_len;

        if offset >= all_bytes.len() {
            break;
        }
    }

    println!("\n=== SEGMENT ANALYSIS ===");
    println!("Segments found: {}", segments_found);
    println!("Bytes parsed: {}", offset);
    println!("Bytes remaining: {}", all_bytes.len() - offset);

    // CRITICAL ASSERTION: Should read all 3 segments
    assert!(
        all_bytes.len() >= expected_min_bytes,
        "❌ FAIL: Only read {} bytes (expected at least {}). This means HLS/SyncReader only loaded {} segments instead of 3!",
        all_bytes.len(),
        expected_min_bytes,
        segments_found
    );

    assert_eq!(
        segments_found, 3,
        "❌ FAIL: Only found {} segments, expected 3. HLS or SyncReader stopped early!",
        segments_found
    );

    println!("\n✅ PASS: SyncReader successfully read all {} bytes from all 3 HLS segments!", all_bytes.len());

    cancel_token.cancel();
}
