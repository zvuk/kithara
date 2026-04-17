#![forbid(unsafe_code)]

use std::sync::Arc;

use bytes::Bytes;
use kithara::{
    drm::{DecryptContext, aes128_cbc_process_chunk},
    hls::{HlsError, HlsResult, KeyProcessorRegistry, KeyProcessorRule},
};
use kithara_integration_tests::hls_fixture::*;
use kithara_platform::time::Duration;
use url::Url;

fn registry_for_host(host: &str, processor: kithara::hls::KeyProcessor) -> KeyProcessorRegistry {
    let mut reg = KeyProcessorRegistry::new();
    reg.add(KeyProcessorRule::new([host], processor));
    reg
}

// Test Cases

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn fetch_and_cache_key(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = test_server.await;
    let key_manager = test_key_manager(&assets_fixture, None);
    let key_url = server.url("/key.bin");

    let key = key_manager.get_raw_key(&key_url, None).await?;
    assert!(!key.is_empty(), "fetched key must not be empty");

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn key_processor_applied(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = test_server.await;

    let processor: kithara::hls::KeyProcessor = Arc::new(|key: Bytes| {
        let mut processed = Vec::new();
        processed.extend_from_slice(b"processed:");
        processed.extend_from_slice(&key);
        Ok(Bytes::from(processed))
    });

    let key_url = server.url("/key.bin");
    let host = key_url.host_str().expect("host").to_string();
    let registry = registry_for_host(&host, processor);
    let key_manager = test_key_manager(&assets_fixture, Some(registry));

    let key: Bytes = key_manager.get_raw_key(&key_url, None).await?;
    assert!(key.starts_with(b"processed:"));

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn key_manager_with_different_processors(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = test_server.await;

    let uppercase_processor: kithara::hls::KeyProcessor = Arc::new(|key: Bytes| {
        let upper = key.to_ascii_uppercase();
        Ok(Bytes::from(upper))
    });

    let key_url = server.url("/key.bin");
    let host = key_url.host_str().expect("host").to_string();
    let registry = registry_for_host(&host, uppercase_processor);
    let key_manager = test_key_manager(&assets_fixture, Some(registry));

    let key: Bytes = key_manager.get_raw_key(&key_url, None).await?;
    assert!(key.is_ascii());

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn key_manager_error_handling(assets_fixture: TestAssets) -> HlsResult<()> {
    let key_manager = test_key_manager(&assets_fixture, None);

    let invalid_url = Url::parse("http://invalid-domain-that-does-not-exist-12345.com/master.m3u8")
        .map_err(|e| HlsError::InvalidUrl(e.to_string()))?;

    let result: HlsResult<Bytes> = key_manager.get_raw_key(&invalid_url, None).await;
    assert!(result.is_err(), "invalid URL should fail, got Ok");

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn key_manager_caching_behavior(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = test_server.await;
    let key_manager = test_key_manager(&assets_fixture, None);
    let key_url = server.url("/key.bin");

    let key1: Bytes = key_manager.get_raw_key(&key_url, None).await?;
    let key2 = key_manager.get_raw_key(&key_url, None).await?;

    assert_eq!(key1, key2);

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn unmatched_domain_uses_raw_key(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = test_server.await;

    // Processor registered for a domain that doesn't match — key should
    // pass through unmodified.
    let sentinel_processor: kithara::hls::KeyProcessor =
        Arc::new(|_key: Bytes| Ok(Bytes::from_static(b"MODIFIED")));
    let registry = registry_for_host("other.example", sentinel_processor);
    let key_manager = test_key_manager(&assets_fixture, Some(registry));

    let key_url = server.url("/key.bin");
    let key = key_manager.get_raw_key(&key_url, None).await?;
    assert_ne!(&key[..], b"MODIFIED");

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn aes128_key_decrypts_ciphertext(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    net_fixture: kithara::net::HttpClient,
) -> HlsResult<()> {
    let server = test_server.await;
    let net = net_fixture;
    let key_manager = test_key_manager(&assets_fixture, None);

    let key_url = server.url("/aes/key.bin");
    let cipher_url = server.url("/aes/seg0.bin");
    let iv = aes128_iv();

    let key_bytes = key_manager.get_raw_key(&key_url, Some(iv)).await?;
    let mut key = [0u8; 16];
    key.copy_from_slice(&key_bytes[..16]);

    let cipher = net.get_bytes(cipher_url, None).await?;
    let mut ctx = DecryptContext::new(key, iv);
    let mut output = vec![0u8; cipher.len()];
    let written = aes128_cbc_process_chunk(&cipher, &mut output, &mut ctx, true)
        .map_err(HlsError::KeyProcessing)?;

    assert!(output[..written].starts_with(aes128_plaintext_segment().as_slice()));

    Ok(())
}
