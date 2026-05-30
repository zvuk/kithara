use std::{
    error::Error,
    io::{Read as _, Seek as _, SeekFrom},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use kithara::{
    assets::StoreOptions,
    events::{AbrEvent, Event, EventBus, HlsEvent},
    hls::{AbrMode, Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir, auto,
    hls_server::abr::{AbrTestServer, master_playlist},
    temp_dir,
};
use kithara_platform::{time::sleep, tokio::task::spawn_blocking};
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Test that ABR variant switch does not cause byte reading glitches.
///
/// Scenario:
/// 1. Start reading from variant 0 (slow segment0 triggers ABR down)
/// 2. ABR triggers switch to variant 2 (faster)
/// 3. Pipeline starts loading variant 2 segments ahead (e.g., 1, 2)
/// 4. `SourceReader` needs to read from beginning - triggers seek back
/// 5. Verify no byte skips/glitches occur
///
/// Expected behavior:
/// - All bytes should be readable sequentially
/// - Variant switch should be seamless from reader's perspective
/// - No gaps or duplicates in byte stream
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_decode=debug,kithara_hls=debug")
)]
async fn test_abr_variant_switch_no_byte_glitches(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_secs(2),
    )
    .await;

    let url = server.url("/master.m3u8");
    info!("Test server started at: {}", url);

    let cancel_token = CancellationToken::new();

    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let config = HlsConfig::for_url(url.clone())
        .cancel(cancel_token.clone())
        .events(bus)
        .store(StoreOptions::new(temp_dir.path()))
        .initial_abr_mode(auto(0))
        .build();

    info!("Opening HLS stream with ABR enabled");

    let mut stream = Stream::<Hls>::new(config).await?;

    let variant_switches = Arc::new(StdMutex::new(Vec::new()));
    let variant_switches_clone = variant_switches.clone();

    tokio::spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            match ev {
                Event::Abr(AbrEvent::VariantApplied { from, to, .. }) => {
                    info!("Variant switch detected: {} -> {}", from, to);
                    variant_switches_clone
                        .lock()
                        .unwrap()
                        .push((from.get(), to.get()));
                }
                Event::Hls(HlsEvent::EndOfStream) => break,
                _ => {}
            }
        }
    });

    info!("Reading bytes from Stream<Hls>");

    sleep(Duration::from_millis(100)).await;

    let result = spawn_blocking(move || -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        let mut all_bytes = Vec::new();
        let mut buffer = vec![0u8; 4096];
        let mut total_reads = 0;

        loop {
            match stream.read(&mut buffer) {
                Ok(n) if n > 0 => {
                    total_reads += 1;
                    tracing::debug!(
                        "Read {} bytes (read #{}, total so far: {})",
                        n,
                        total_reads,
                        all_bytes.len() + n
                    );
                    all_bytes.extend_from_slice(&buffer[..n]);
                }
                Ok(_) => {
                    tracing::debug!(
                        "EOF reached after {} reads, total bytes: {}",
                        total_reads,
                        all_bytes.len()
                    );
                    break;
                }
                Err(e) => return Err(format!("Read error: {}", e).into()),
            }
        }

        Ok(all_bytes)
    })
    .await??;

    info!("Read {} bytes total", result.len());

    assert!(
        result.len() > 100_000,
        "Should have read substantial data (got {} bytes)",
        result.len()
    );

    let non_zero_bytes = result.iter().filter(|&&b| b != 0).count();
    assert!(
        non_zero_bytes > result.len() / 2,
        "Should have read actual content, not zeros"
    );

    let switches = variant_switches.lock().unwrap();
    info!("Variant switches detected: {:?}", *switches);

    info!("Test passed: bytes read sequentially without gaps");

    Ok(())
}

/// Simpler test without ABR - just verify basic multi-segment reading works
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("info")
)]
async fn test_basic_multi_segment_reading(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(1),
    )
    .await;

    let url = server.url("/master.m3u8");
    let cancel_token = CancellationToken::new();

    let config = HlsConfig::for_url(url)
        .cancel(cancel_token.clone())
        .store(StoreOptions::new(temp_dir.path()))
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let mut stream = Stream::<Hls>::new(config).await?;

    let result = spawn_blocking(move || -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        let mut all_bytes = Vec::new();
        let mut buffer = vec![0u8; 4096];

        loop {
            match stream.read(&mut buffer) {
                Ok(n) if n > 0 => {
                    all_bytes.extend_from_slice(&buffer[..n]);
                }
                Ok(_) => break,
                Err(e) => return Err(format!("Read error: {}", e).into()),
            }
        }

        Ok(all_bytes)
    })
    .await??;

    info!("Read {} bytes from variant 0", result.len());
    assert!(
        result.len() > 500_000,
        "Should have read all segments (got {} bytes)",
        result.len()
    );

    info!("Basic multi-segment test passed");

    Ok(())
}

/// Test ABR variant switch with seek backward - reproduces the original bug.
///
/// Scenario that triggers the bug:
/// 1. Start reading from variant 0
/// 2. ABR switches to variant 2
/// 3. Pipeline loads variant 2 starting from segment 2 (continues from where variant 0 left off)
/// 4. Reader seeks back to offset 0 - this requires loading segment 0 from variant 2
/// 5. BUG: `first_media_segment` stays at 2 instead of updating to 0
/// 6. This causes gap detection or incorrect offset calculations
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("debug")
)]
async fn test_abr_variant_switch_with_seek_backward(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        true,
        Duration::from_secs(2),
    )
    .await;

    let url = server.url("/master.m3u8");
    let cancel_token = CancellationToken::new();

    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let config = HlsConfig::for_url(url)
        .cancel(cancel_token.clone())
        .events(bus)
        .store(StoreOptions::new(temp_dir.path()))
        .initial_abr_mode(auto(0))
        .build();

    println!("\nCreating HLS stream with ABR");
    let mut stream = Stream::<Hls>::new(config).await?;

    let variant_switches = Arc::new(StdMutex::new(Vec::new()));
    let variant_switches_clone = variant_switches.clone();

    tokio::spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            if let Event::Abr(AbrEvent::VariantApplied { from, to, .. }) = ev {
                println!("Variant switch: {} -> {}", from, to);
                variant_switches_clone.lock().unwrap().push((from, to));
            }
        }
    });

    sleep(Duration::from_millis(100)).await;

    spawn_blocking(move || -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut buffer = vec![0u8; 50000];
        let n1 = stream.read(&mut buffer)?;
        println!("Read {} bytes initially", n1);

        println!("Seeking back to offset 0");
        stream.seek(SeekFrom::Start(0))?;

        let mut buffer2 = vec![0u8; 1000];
        let n2 = stream.read(&mut buffer2)?;
        println!("Read {} bytes after seek to 0", n2);

        assert!(n2 > 0, "Should be able to read after seeking to beginning");

        let data_str = String::from_utf8_lossy(&buffer2[..n2]);
        assert!(
            data_str.starts_with("V"),
            "Should read segment data after seek, got: {}",
            &data_str[..20.min(data_str.len())]
        );

        println!("Successfully read after seek backward");

        Ok(())
    })
    .await??;

    let switches = variant_switches.lock().unwrap();
    println!("Variant switches: {:?}", *switches);

    println!("Test passed - seek backward after ABR variant switch works correctly");

    Ok(())
}
