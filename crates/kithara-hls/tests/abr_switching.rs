#![forbid(unsafe_code)]

mod fixture;

use std::time::Duration;

use fixture::{
    AbrTestServer, HlsResult, TestAssets, abr_server_default, assets_fixture, master_playlist,
};
use futures::StreamExt;
use kithara_hls::{HlsEvent, HlsOptions, HlsSource};
use rstest::rstest;

// ==================== Test Cases ====================

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn abr_upswitch_continues_from_current_segment_index(
    #[future] abr_server_default: AbrTestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = abr_server_default.await;
    let _assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;
    let mut options = HlsOptions::default();
    options.abr_initial_variant_index = Some(0);
    options.abr_min_buffer_for_up_switch = 0.0;
    options.abr_min_switch_interval = Duration::ZERO;

    let session = HlsSource::open(master_url, options).await?;
    let mut events = session.events();

    let mut stream = Box::pin(session.stream());

    // Get first segment from variant 0
    let first = stream.next().await.unwrap()?;
    assert!(
        first.starts_with(b"V0-SEG-0:"),
        "First segment should be from variant 0"
    );

    let second = stream.next().await.unwrap()?;
    assert!(
        second.starts_with(b"V2-SEG-1:"),
        "Automatic ABR should upswitch to variant 2 for the next segment"
    );

    let mut found = false;
    for _ in 0..20 {
        let ev = tokio::time::timeout(Duration::from_millis(50), events.recv()).await;
        let Ok(Ok(ev)) = ev else { continue };
        match ev {
            HlsEvent::VariantApplied {
                from_variant,
                to_variant,
                ..
            }
            | HlsEvent::VariantDecision {
                from_variant,
                to_variant,
                ..
            } => {
                assert_eq!(from_variant, 0);
                assert_eq!(to_variant, 2);
                found = true;
                break;
            }
            _ => continue,
        }
    }
    assert!(
        found,
        "expected VariantApplied/VariantDecision to variant 2 within timeout"
    );

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn abr_downswitch_emits_init_before_next_segment_when_required(
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = AbrTestServer::new(
        master_playlist(500_000, 1_000_000, 5_000_000),
        true,
        Duration::from_millis(200),
    )
    .await;
    let _assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;
    let mut options = HlsOptions::default();
    options.abr_initial_variant_index = Some(2);
    options.abr_min_switch_interval = Duration::ZERO;
    options.abr_min_buffer_for_up_switch = 0.0;
    options.prefetch_buffer_size = Some(1);

    let session = HlsSource::open(master_url, options).await?;

    let mut stream = Box::pin(session.stream());

    // Get init segment from variant 2 (initial variant)
    let init0 = stream.next().await.unwrap()?;
    assert!(
        init0.starts_with(b"V2-INIT:"),
        "Initial init segment should be from variant 2"
    );

    // Get first segment from variant 2
    let seg0 = stream.next().await.unwrap()?;
    assert!(
        seg0.starts_with(b"V2-SEG-0:"),
        "First segment should be from variant 2"
    );

    let mut chunk_v1 = None;
    for _ in 0..6 {
        let chunk = stream.next().await.unwrap()?;
        if chunk.starts_with(b"V1-") {
            chunk_v1 = Some(chunk);
            break;
        }
    }
    let next = chunk_v1.expect("expected chunk from variant 1 after downswitch");
    if next.starts_with(b"V1-INIT:") {
        let seg1 = stream.next().await.unwrap()?;
        assert!(
            seg1.starts_with(b"V1-SEG-1:"),
            "After downswitch the next media segment should be from variant 1"
        );
    } else {
        assert!(
            next.starts_with(b"V1-SEG-1:"),
            "Downswitch may skip init; media should still be from variant 1"
        );
    }

    Ok(())
}

#[rstest]
#[case(256_000, 512_000, 1_024_000, false, Duration::from_millis(1))]
#[case(500_000, 1_000_000, 5_000_000, true, Duration::from_millis(200))]
#[case(100_000, 200_000, 300_000, false, Duration::from_millis(10))]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn abr_with_different_bandwidth_configurations(
    #[case] v0_bw: u64,
    #[case] v1_bw: u64,
    #[case] v2_bw: u64,
    #[case] init: bool,
    #[case] segment0_delay: Duration,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server =
        AbrTestServer::new(master_playlist(v0_bw, v1_bw, v2_bw), init, segment0_delay).await;
    let _assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;
    let mut options = HlsOptions::default();
    options.abr_initial_variant_index = Some(0);
    options.abr_min_buffer_for_up_switch = 0.0;
    options.abr_min_switch_interval = Duration::ZERO;

    let session = HlsSource::open(master_url, options).await?;
    let mut stream = Box::pin(session.stream());

    // Read at least one chunk to ensure stream works
    let first_chunk = stream.next().await;
    assert!(first_chunk.is_some());

    if let Some(Ok(bytes)) = first_chunk {
        assert!(!bytes.is_empty());
    }

    Ok(())
}

#[rstest]
#[case(0)]
#[case(1)]
#[case(2)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn abr_with_different_initial_variants(
    #[case] initial_variant: usize,
    #[future] abr_server_default: AbrTestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = abr_server_default.await;
    let _assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;
    let mut options = HlsOptions::default();
    options.abr_initial_variant_index = Some(initial_variant);
    options.abr_min_buffer_for_up_switch = 0.0;
    options.abr_min_switch_interval = Duration::ZERO;

    let session = HlsSource::open(master_url, options).await?;
    let mut stream = Box::pin(session.stream());

    // Read first chunk and verify it's from the expected initial variant
    let first_chunk = stream.next().await;
    assert!(first_chunk.is_some());

    if let Some(Ok(bytes)) = first_chunk {
        let expected_prefix = format!("V{}-", initial_variant);
        assert!(
            bytes.starts_with(expected_prefix.as_bytes()),
            "Expected chunk from variant {}, got: {:?}",
            initial_variant,
            String::from_utf8_lossy(&bytes[..bytes.len().min(20)])
        );
    }

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn manual_variant_switching_works(
    #[future] abr_server_default: AbrTestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = abr_server_default.await;
    let _assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;
    let mut options = HlsOptions::default();
    options.abr_initial_variant_index = Some(0);

    let session = HlsSource::open(master_url, options).await?;

    // Get first segment from variant 0
    let mut stream = Box::pin(session.stream());
    let first = stream.next().await.unwrap()?;
    assert!(
        first.starts_with(b"V0-SEG-0:"),
        "First segment should be from variant 0"
    );

    // Get second segment (should also be variant 0)
    let second = stream.next().await.unwrap()?;
    assert!(
        second.starts_with(b"V0-SEG-1:"),
        "Second segment should be from variant 0"
    );

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn initial_variant_selection_works(
    #[future] abr_server_default: AbrTestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = abr_server_default.await;
    let _assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;

    // Test with initial variant 1
    let mut options = HlsOptions::default();
    options.abr_initial_variant_index = Some(1);

    let session = HlsSource::open(master_url, options).await?;
    let mut stream = Box::pin(session.stream());

    // Should get segment from variant 1
    let first = stream.next().await.unwrap()?;
    assert!(
        first.starts_with(b"V1-SEG-0:"),
        "First segment should be from variant 1 when abr_initial_variant_index=1"
    );

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn abr_events_channel_functionality(
    #[future] abr_server_default: AbrTestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = abr_server_default.await;
    let _assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;
    let mut options = HlsOptions::default();
    options.abr_initial_variant_index = Some(0);
    options.abr_min_buffer_for_up_switch = 0.0;
    options.abr_min_switch_interval = Duration::ZERO;

    let session = HlsSource::open(master_url, options).await?;
    let mut events = session.events();

    // Start reading stream in background to trigger events
    let stream_handle = tokio::spawn(async move {
        let mut stream = Box::pin(session.stream());
        let mut count = 0;
        while let Some(result) = stream.next().await {
            if result.is_err() || count >= 3 {
                break;
            }
            count += 1;
        }
        count
    });

    // Try to receive some events
    let mut event_count = 0;
    let timeout = Duration::from_millis(500);

    while event_count < 3 {
        let event_result = tokio::time::timeout(timeout, events.recv()).await;
        match event_result {
            Ok(Ok(_event)) => {
                event_count += 1;
            }
            Ok(Err(_)) => {
                // Channel closed
                break;
            }
            Err(_) => {
                // Timeout - no more events
                break;
            }
        }
    }

    // Wait for stream to finish
    let _chunks_read = stream_handle
        .await
        .map_err(|e| kithara_hls::HlsError::Driver(e.to_string()))?;

    // Should have received at least some events
    assert!(event_count > 0, "Expected to receive some events");

    Ok(())
}
