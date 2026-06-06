use std::io::Read;

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir,
    hls_server::abr::{AbrTestServer, master_playlist},
    temp_dir,
};
use kithara_platform::{
    CancellationToken,
    time::{Duration, sleep},
    tokio::task::spawn_blocking,
};

#[kithara::test(
    flash(false),
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_sync_reader_reads_all_bytes_from_hls(temp_dir: TestTempDir) {
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(10),
    )
    .await;

    let url = server.url("/master.m3u8");
    let cancel_token = CancellationToken::default();

    let config = HlsConfig::for_url(url.clone())
        .cancel(cancel_token.clone())
        .store(StoreOptions::new(temp_dir.path()))
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    sleep(Duration::from_secs(2)).await;

    let mut all_bytes = Vec::new();
    let mut read_buf = vec![0u8; 64 * 1024];
    let mut total_reads = 0;

    let result = spawn_blocking(move || {
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

    let expected_min_bytes = 500_000;

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

    assert!(
        all_bytes.len() >= expected_min_bytes,
        "FAIL: Only read {} bytes (expected at least {}). This means HLS failed to load segments!",
        all_bytes.len(),
        expected_min_bytes
    );

    assert!(
        segments_found >= 3,
        "FAIL: Only found {} segments, expected at least 3. HLS stopped early!",
        segments_found
    );

    cancel_token.cancel();
}
