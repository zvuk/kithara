#![forbid(unsafe_code)]

//! Integration tests for new CachedLoader architecture.
//!
//! These tests verify that CachedLoader<FetchLoader> works correctly
//! with real HLS playlists and network fetching.

use std::time::Duration;

use kithara_assets::StoreOptions;
use kithara_hls::{Hls, HlsParams};
use kithara_stream::Source;
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::EnvFilter;

use super::fixture::TestServer;

// ==================== Fixtures ====================

#[fixture]
fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[fixture]
fn hls_params(temp_dir: TempDir) -> HlsParams {
    HlsParams::new(StoreOptions::new(temp_dir.path())).with_cancel(CancellationToken::new())
}

#[fixture]
fn minimal_tracing_setup() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("debug".parse().unwrap()))
        .with_test_writer()
        .try_init();
}

// ==================== Basic Tests ====================

/// CLInt-1: Basic session creation with new architecture
#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn test_cached_loader_basic_creation(
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!(
        "Testing CachedLoader creation with URL: {}",
        test_stream_url
    );

    // Open using new architecture
    let source = Hls::open_v2(test_stream_url.clone(), hls_params).await?;
    info!("CachedLoader opened successfully");

    // Test basic Source trait methods
    let len = source.len();
    info!("Source len: {:?}", len);

    let media_info = source.media_info();
    info!("Media info: {:?}", media_info);

    Ok(())
}

/// CLInt-2: Read first segment
#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn test_cached_loader_read_first_segment(
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;

    let source = Hls::open_v2(test_stream_url, hls_params).await?;

    // Wait for first segment range
    let wait_result = source.wait_range(0..1000).await?;
    info!("wait_range result: {:?}", wait_result);

    // Read some bytes from beginning
    let mut buf = vec![0u8; 1000];
    let bytes_read = source.read_at(0, &mut buf).await?;
    info!("Read {} bytes from offset 0", bytes_read);

    Ok(())
}

/// CLInt-3: Sequential reads
#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn test_cached_loader_sequential_reads(
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;

    let source = Hls::open_v2(test_stream_url, hls_params).await?;

    let mut buf = vec![0u8; 1000];

    // Sequential reads
    for i in 0..5 {
        let offset = i * 1000;
        source.wait_range(offset..offset + 1000).await?;
        let bytes_read = source.read_at(offset, &mut buf).await?;
        info!(
            "Read iteration {}: {} bytes at offset {}",
            i, bytes_read, offset
        );
    }

    Ok(())
}

/// CLInt-4: Random access (seek)
#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn test_cached_loader_random_access(
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;

    let source = Hls::open_v2(test_stream_url, hls_params).await?;

    let mut buf = vec![0u8; 1000];

    // Jump to different offsets
    let offsets = vec![0, 100_000, 50_000, 200_000, 10_000];

    for offset in offsets {
        source.wait_range(offset..offset + 1000).await?;
        let bytes_read = source.read_at(offset, &mut buf).await?;
        info!("Random read: {} bytes at offset {}", bytes_read, offset);
    }

    Ok(())
}

/// CLInt-5: Variant switch simulation
#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn test_cached_loader_variant_switch(
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;

    let source = Hls::open_v2(test_stream_url, hls_params).await?;

    let mut buf = vec![0u8; 1000];

    // Read from variant 0
    info!("Reading from variant 0");
    source.wait_range(0..1000).await?;
    let bytes_read = source.read_at(0, &mut buf).await?;
    info!("Read {} bytes from variant 0", bytes_read);

    // Switch to variant 1
    info!("Switching to variant 1");
    source.set_current_variant(1);

    // Read from variant 1
    source.wait_range(100_000..101_000).await?;
    let bytes_read = source.read_at(100_000, &mut buf).await?;
    info!("Read {} bytes from variant 1", bytes_read);

    Ok(())
}

// ==================== Encryption Tests ====================

/// CLInt-6: AES-128 encrypted segment decryption
#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn test_cached_loader_aes128_decryption(
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master-encrypted.m3u8")?;
    info!(
        "Testing CachedLoader with AES-128 encrypted content: {}",
        test_stream_url
    );

    // Open encrypted stream using new architecture
    let source = Hls::open_v2(test_stream_url.clone(), hls_params).await?;
    info!("CachedLoader opened encrypted stream");

    // Wait for first encrypted segment
    let wait_result = source.wait_range(0..1000).await?;
    info!("wait_range result for encrypted segment: {:?}", wait_result);

    // Read from encrypted segment - should be automatically decrypted
    let mut buf = vec![0u8; 1000];
    let bytes_read = source.read_at(0, &mut buf).await?;
    info!(
        "Read {} bytes from encrypted segment (decrypted)",
        bytes_read
    );

    // Verify decrypted data contains expected plaintext prefix
    assert!(
        bytes_read > 0,
        "Should read some bytes from encrypted segment"
    );

    // The plaintext segment should start with "V0-SEG-0:DRM-PLAINTEXT"
    let decrypted_prefix = &buf[..bytes_read.min(22)];
    let expected_prefix = b"V0-SEG-0:DRM-PLAINTEXT";
    assert!(
        decrypted_prefix.starts_with(expected_prefix),
        "Decrypted data should start with plaintext marker, got: {:?}",
        String::from_utf8_lossy(decrypted_prefix)
    );

    info!("✅ AES-128 decryption successful! Plaintext verified.");

    Ok(())
}
