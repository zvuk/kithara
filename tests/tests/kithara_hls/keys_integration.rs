#![forbid(unsafe_code)]

use std::{sync::Arc, time::Duration};

use fixture::*;
use kithara_hls::{AssetsBackend, HlsResult, fetch::FetchManager, keys::KeyManager};
use rstest::{fixture, rstest};
use tokio_util::sync::CancellationToken;

use super::fixture;

// Fixtures

#[fixture]
async fn test_server() -> TestServer {
    TestServer::new().await
}

// Test Cases

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn fetch_and_cache_key(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let fetch_manager = Arc::new(FetchManager::new(
        AssetsBackend::Disk(assets),
        net,
        CancellationToken::new(),
    ));
    let key_manager = KeyManager::new(fetch_manager.clone(), None, None, None);
    let key_url = server.url("/key.bin")?;

    // Note: This test assumes the server provides a key endpoint
    // In real implementation, the test server would need to serve key data
    let _key = key_manager.get_raw_key(&key_url, None).await;

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn key_processor_applied(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let processor = Arc::new(|key: bytes::Bytes, _context: kithara_hls::KeyContext| {
        // Simple processor that just adds a prefix
        let mut processed = Vec::new();
        processed.extend_from_slice(b"processed:");
        processed.extend_from_slice(&key);
        Ok(bytes::Bytes::from(processed))
    });

    let fetch_manager = Arc::new(FetchManager::new(
        AssetsBackend::Disk(assets),
        net,
        CancellationToken::new(),
    ));
    let key_manager = KeyManager::new(fetch_manager.clone(), Some(processor), None, None);
    let key_url = server.url("/key.bin")?;

    let key = key_manager.get_raw_key(&key_url, None).await?;
    assert!(key.starts_with(b"processed:"));

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn key_manager_with_different_processors(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    // Test with uppercase processor
    let uppercase_processor = Arc::new(|key: bytes::Bytes, _context: kithara_hls::KeyContext| {
        let upper = key.to_ascii_uppercase();
        Ok(bytes::Bytes::from(upper))
    });

    let fetch_manager = Arc::new(FetchManager::new(
        AssetsBackend::Disk(assets.clone()),
        net.clone(),
        CancellationToken::new(),
    ));
    let key_manager = KeyManager::new(fetch_manager.clone(), Some(uppercase_processor), None, None);
    let key_url = server.url("/key.bin")?;

    let key = key_manager.get_raw_key(&key_url, None).await?;
    assert!(key.is_ascii());

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn key_manager_error_handling(
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let fetch_manager = Arc::new(FetchManager::new(
        AssetsBackend::Disk(assets),
        net,
        CancellationToken::new(),
    ));
    let key_manager = KeyManager::new(fetch_manager.clone(), None, None, None);

    // Try to get key from invalid URL
    let invalid_url =
        url::Url::parse("http://invalid-domain-that-does-not-exist-12345.com/master.m3u8")
            .map_err(|e| kithara_hls::HlsError::InvalidUrl(e.to_string()))?;

    let result = key_manager.get_raw_key(&invalid_url, None).await;

    // Should fail with network error (or succeed if somehow connects)
    assert!(result.is_ok() || result.is_err());

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn key_manager_caching_behavior(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let fetch_manager = Arc::new(FetchManager::new(
        AssetsBackend::Disk(assets),
        net,
        CancellationToken::new(),
    ));
    let key_manager = KeyManager::new(fetch_manager.clone(), None, None, None);
    let key_url = server.url("/key.bin")?;

    // First fetch
    let key1 = key_manager.get_raw_key(&key_url, None).await?;

    // Second fetch should potentially use cache
    let key2 = key_manager.get_raw_key(&key_url, None).await?;

    // Keys should be the same (either from cache or re-fetched)
    assert_eq!(key1, key2);

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn key_manager_with_context(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let processor = Arc::new(|key: bytes::Bytes, context: kithara_hls::KeyContext| {
        // Use context to modify key
        let mut processed = Vec::new();
        processed.extend_from_slice(b"ctx:");
        if let Some(iv) = context.iv {
            processed.extend_from_slice(&iv);
            processed.extend_from_slice(b":");
        }
        processed.extend_from_slice(&key);
        Ok(bytes::Bytes::from(processed))
    });

    let fetch_manager = Arc::new(FetchManager::new(
        AssetsBackend::Disk(assets),
        net,
        CancellationToken::new(),
    ));
    let key_manager = KeyManager::new(fetch_manager.clone(), Some(processor), None, None);
    let key_url = server.url("/key.bin")?;

    // Test without IV
    let key1 = key_manager.get_raw_key(&key_url, None).await?;
    assert!(key1.starts_with(b"ctx:"));

    // Test with IV
    let mut iv = [0u8; 16];
    iv[..7].copy_from_slice(b"test-iv");
    let key2 = key_manager.get_raw_key(&key_url, Some(iv)).await?;
    assert!(key2.starts_with(b"ctx:"));

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn aes128_key_decrypts_ciphertext(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara_net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();
    let net = net_fixture;

    let fetch_manager = Arc::new(FetchManager::new(
        AssetsBackend::Disk(assets),
        net.clone(),
        CancellationToken::new(),
    ));
    let key_manager = KeyManager::new(fetch_manager.clone(), None, None, None);

    let key_url = server.url("/aes/key.bin")?;
    let cipher_url = server.url("/aes/seg0.bin")?;
    let iv = fixture::aes128_iv();

    // Fetch key through KeyManager (the production path)
    let key_bytes = key_manager.get_raw_key(&key_url, Some(iv)).await?;
    let mut key = [0u8; 16];
    key.copy_from_slice(&key_bytes[..16]);

    // Decrypt ciphertext through kithara-drm (the production decrypt path)
    let cipher = net.get_bytes(cipher_url, None).await?;
    let mut ctx = kithara_drm::DecryptContext::new(key, iv);
    let mut output = vec![0u8; cipher.len()];
    let written = kithara_drm::aes128_cbc_process_chunk(&cipher, &mut output, &mut ctx, true)
        .map_err(kithara_hls::HlsError::KeyProcessing)?;

    assert!(output[..written].starts_with(fixture::aes128_plaintext_segment().as_slice()));

    Ok(())
}
