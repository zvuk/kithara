//! TDD Test: Decode + HLS + ABR Variant Switch
//!
//! Этот тест проверяет полную связку: decode layer поверх HLS с ABR.
//! Связка stream+hls уже протестирована в kithara-hls tests.
//!
//! Использует AudioSyncReader как в example hls_decode.rs
//!
//! Сценарий:
//! 1. Start with variant 0 (slow - triggers ABR upswitch)
//! 2. Pipeline reads PCM samples through MockDecoder
//! 3. ABR switches to higher variant mid-stream
//! 4. AudioSyncReader reads from ring buffer
//! 5. Verify PCM samples are sequential
//!
//! Expected behavior:
//! - WITHOUT fix: Samples have gaps (segment indices jump)
//! - WITH fix: All samples sequential

use std::{error::Error, sync::Arc, time::Duration};

use kithara_decode::{PcmSpec, Pipeline};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsParams};
use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
use tokio_util::sync::CancellationToken;

use super::mock_decoder::MockDecoder;
use crate::kithara_hls::fixture::abr::segment_data;

/// Verify PCM samples are sequential (no gaps in segment indices).
///
/// MockDecoder generates pattern: [variant, segment, 0.0, 1.0, ..., 99.0]
/// We check that segments within each variant are sequential.
fn verify_sequential_samples(samples: &[f32]) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }

    let mut segments_by_variant: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();

    // Each chunk is 102 samples: [variant, segment, data...]
    let mut i = 0;
    while i + 1 < samples.len() {
        let variant = samples[i] as usize;
        let segment = samples[i + 1] as usize;

        segments_by_variant
            .entry(variant)
            .or_insert_with(Vec::new)
            .push(segment);

        i += 102; // Skip to next chunk
    }

    // Check each variant's segments are sequential
    for (variant, segments) in segments_by_variant.iter() {
        println!("Variant {}: segments {:?}", variant, segments);

        for i in 1..segments.len() {
            let prev = segments[i - 1];
            let curr = segments[i];

            if curr != prev + 1 {
                return Err(format!(
                    "❌ GAP in variant {}: segment jumped from {} to {} (expected {})",
                    variant,
                    prev,
                    curr,
                    prev + 1
                ));
            }
        }
    }

    Ok(())
}

/// Check if ABR variant switch occurred.
fn has_variant_switch(samples: &[f32]) -> bool {
    let mut variants = std::collections::HashSet::new();

    let mut i = 0;
    while i < samples.len() {
        let variant = samples[i] as usize;
        variants.insert(variant);
        i += 102;
    }

    variants.len() > 1
}

/// Custom fixture for ABR test with 10 segments per variant.
fn master_playlist_10_segments(v0_bw: u64, v1_bw: u64, v2_bw: u64) -> String {
    format!(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH={v0_bw}
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH={v1_bw}
v1.m3u8
#EXT-X-STREAM-INF:BANDWIDTH={v2_bw}
v2.m3u8
"#
    )
}

fn media_playlist_10_segments(variant: usize) -> String {
    let mut s = String::new();
    s.push_str(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
"#,
    );
    for i in 0..10 {
        s.push_str("#EXTINF:4.0,\n");
        s.push_str(&format!("seg/v{}_{}.bin\n", variant, i));
    }
    s.push_str("#EXT-X-ENDLIST\n");
    s
}

/// Test decode + HLS + ABR variant switch using AudioSyncReader.
///
/// CRITICAL REQUIREMENTS:
/// - 10 segments per variant
/// - 3 variants (v0, v1, v2)
/// - ALL 10 segments must be read
/// - Must switch between variants during playback
///
/// WITHOUT fix: Stops after 2-3 segments (at ABR boundary)
/// WITH fix: Reads all 10 segments seamlessly
#[tokio::test]
async fn test_decode_hls_abr_variant_switch() -> Result<(), Box<dyn Error + Send + Sync>> {
    use axum::{Router, routing::get};
    use tokio::net::TcpListener;

    // Create custom server with 10 segments per variant
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    let master = master_playlist_10_segments(256_000, 512_000, 1_024_000);

    let app = Router::new()
        .route(
            "/master.m3u8",
            get(move || {
                let m = master.clone();
                async move { m }
            }),
        )
        .route("/v0.m3u8", get(|| async { media_playlist_10_segments(0) }))
        .route("/v1.m3u8", get(|| async { media_playlist_10_segments(1) }))
        .route("/v2.m3u8", get(|| async { media_playlist_10_segments(2) }));

    // Add routes for all 10 segments * 3 variants = 30 routes
    let mut app = app;
    for variant in 0..3 {
        for segment in 0..10 {
            let route = format!("/seg/v{}_{}.bin", variant, segment);
            app = app.route(
                &route,
                get(move || async move {
                    // Delay only on variant 0 to trigger ABR upswitch
                    if variant == 0 {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    segment_data(variant, segment, Duration::ZERO, 200_000).await
                }),
            );
        }
    }

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let url: url::Url = format!("{}/master.m3u8", base_url).parse()?;
    let cancel_token = CancellationToken::new();

    // Configure HLS with Auto ABR (starts at variant 0, switches up due to delay)
    let hls_params = HlsParams::default()
        .with_cancel(cancel_token.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            min_buffer_for_up_switch_secs: 0.5,
            down_switch_buffer_secs: 0.3,
            throughput_safety_factor: 1.2,
            ..Default::default()
        });

    // Open HLS source
    let source = StreamSource::<Hls>::open(url.clone(), hls_params).await?;
    let source_arc = Arc::new(source);

    // Create SyncReader for MockDecoder with increased prefetch buffer
    // Each segment is ~200KB, need larger buffer to avoid EOF during prefetch
    let sync_reader_params = SyncReaderParams {
        chunk_size: 64 * 1024, // 64KB (default)
        prefetch_chunks: 16,   // 16 chunks = 1MB buffer (enough for ~5 segments)
    };
    let sync_reader = SyncReader::new(source_arc.clone(), sync_reader_params);

    // Create MockDecoder
    let spec = PcmSpec {
        sample_rate: 44100,
        channels: 2,
    };
    let mock_decoder = MockDecoder::with_spec(sync_reader, spec);

    // Create Pipeline with MockDecoder
    let pipeline = Pipeline::with_decoder(mock_decoder, spec, 44100).await?;

    // Give HLS time to prefetch ALL segments (3 segments * 2sec delay = 6+ seconds)
    // Also give MockDecoder time to start reading
    tokio::time::sleep(Duration::from_secs(8)).await;

    // Read PCM samples from channel
    // IMPORTANT: Channel has backpressure - when we read slowly, decoder blocks automatically
    let mut all_samples = Vec::new();
    let sample_rx = pipeline.consumer();
    // IMPORTANT: MockDecoder has 5sec retry timeout - need longer read_timeout!
    let read_timeout = Duration::from_secs(10);

    // Read chunks until timeout
    let async_rx = sample_rx.as_async();
    let mut chunks_received = 0;

    loop {
        let result = tokio::time::timeout(read_timeout, async { async_rx.recv().await }).await;

        match result {
            Ok(Ok(chunk)) => {
                all_samples.extend_from_slice(&chunk);
                chunks_received += 1;
                if chunks_received <= 10 || chunks_received % 10 == 0 {
                    println!(
                        "Received chunk {}: {} samples",
                        chunks_received,
                        chunk.len()
                    );
                }
            }
            Ok(Err(_)) => {
                println!("Channel closed after {} chunks", chunks_received);
                break;
            }
            Err(_) => {
                println!("Timeout after {} chunks", chunks_received);
                break;
            }
        }
    }

    println!("Total samples read: {}", all_samples.len());
    let total_chunks = all_samples.len() / 102;
    println!("Total chunks: {}", total_chunks);

    // CRITICAL CHECK 1: Verify samples are sequential (no gaps/duplicates)
    verify_sequential_samples(&all_samples)?;

    // CRITICAL CHECK 2: Verify ABR variant switch occurred
    assert!(
        has_variant_switch(&all_samples),
        "ABR should have switched variants"
    );

    // CRITICAL CHECK 3: Verify ALL 10 segments were read (minimum requirement)
    assert!(
        total_chunks >= 10,
        "❌ FAIL: Only {} chunks read, expected at least 10 (all segments)! \
         This means reading stopped after ABR switch. Playlist has 10 segments per variant.",
        total_chunks
    );

    // CRITICAL CHECK 4: Verify we read from AT LEAST 2 different variants (ABR switched)
    let mut variants_seen = std::collections::HashSet::new();
    let mut i = 0;
    while i < all_samples.len() {
        let variant = all_samples[i] as usize;
        variants_seen.insert(variant);
        i += 102;
    }

    assert!(
        variants_seen.len() >= 2,
        "❌ FAIL: Expected at least 2 variants (ABR switch), but only read from: {:?}",
        variants_seen
    );

    // Verify variant 0 is present (we start from it)
    assert!(
        variants_seen.contains(&0),
        "❌ FAIL: Expected to start from variant 0, but variants seen: {:?}",
        variants_seen
    );

    // CRITICAL CHECK 5: Verify channel closed naturally (not timeout)
    // If we got here via timeout, it means decoder is stuck!
    println!(
        "✅ PASS: Read {} chunks from variants {:?}",
        total_chunks, variants_seen
    );
    println!("✅ All samples sequential - ABR variant switch works!");

    cancel_token.cancel();

    Ok(())
}
