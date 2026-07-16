#![forbid(unsafe_code)]

use std::collections::HashMap;

use bytes::Bytes;
use kithara::{
    drm::{DecryptContext, KeyRequest, KeyRequestFactory, aes128_cbc_process_chunk},
    hls::{HlsError, HlsResult, KeyProcessorRegistry, KeyProcessorRule},
    platform::{sync::Arc, time::Duration},
};
use kithara_integration_tests::{hls_fixture::*, hls_server::*};
use url::Url;

fn registry_for_host(host: &str, processor: kithara::hls::KeyProcessor) -> KeyProcessorRegistry {
    let factory: KeyRequestFactory =
        Arc::new(move || KeyRequest::new(HashMap::new(), Arc::clone(&processor)));
    let mut reg = KeyProcessorRegistry::new();
    reg.add(KeyProcessorRule::new([host], factory));
    reg
}

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
    let key_store = test_key_store(&assets_fixture, None);
    let key_url = server.url("/aes/key.bin");

    let key = key_store.get_raw_key(&key_url, None).await?;
    assert_eq!(key.as_ref(), b"0123456789abcdef");

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

    let processor: kithara::hls::KeyProcessor =
        Arc::new(|key: Bytes| Ok(Bytes::from(key.to_ascii_uppercase())));

    let key_url = server.url("/aes/key.bin");
    let host = key_url.host_str().expect("host").to_string();
    let registry = registry_for_host(&host, processor);
    let key_store = test_key_store(&assets_fixture, Some(registry));

    let key: Bytes = key_store.get_raw_key(&key_url, None).await?;
    assert_eq!(key.as_ref(), b"0123456789ABCDEF");

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn key_store_with_different_processors(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = test_server.await;

    let reverse_processor: kithara::hls::KeyProcessor = Arc::new(|key: Bytes| {
        let reversed = key.iter().rev().copied().collect::<Vec<_>>();
        Ok(Bytes::from(reversed))
    });

    let key_url = server.url("/aes/key.bin");
    let host = key_url.host_str().expect("host").to_string();
    let registry = registry_for_host(&host, reverse_processor);
    let key_store = test_key_store(&assets_fixture, Some(registry));

    let key: Bytes = key_store.get_raw_key(&key_url, None).await?;
    assert_eq!(key.as_ref(), b"fedcba9876543210");

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn key_store_error_handling(assets_fixture: TestAssets) -> HlsResult<()> {
    let key_store = test_key_store(&assets_fixture, None);

    let invalid_url = Url::parse("http://127.0.0.1:9/master.m3u8")
        .map_err(|e| HlsError::InvalidUrl(e.to_string()))?;

    let result: HlsResult<Bytes> = key_store.get_raw_key(&invalid_url, None).await;
    assert!(result.is_err(), "invalid URL should fail, got Ok");

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn key_store_caching_behavior(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = test_server.await;
    let key_store = test_key_store(&assets_fixture, None);
    let key_url = server.url("/aes/key.bin");

    let key1: Bytes = key_store.get_raw_key(&key_url, None).await?;
    let key2 = key_store.get_raw_key(&key_url, None).await?;

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

    let sentinel_processor: kithara::hls::KeyProcessor =
        Arc::new(|_key: Bytes| Ok(Bytes::from_static(b"MODIFIED")));
    let registry = registry_for_host("other.example", sentinel_processor);
    let key_store = test_key_store(&assets_fixture, Some(registry));

    let key_url = server.url("/aes/key.bin");
    let key = key_store.get_raw_key(&key_url, None).await?;
    assert_eq!(key.as_ref(), b"0123456789abcdef");

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
    let key_store = test_key_store(&assets_fixture, None);

    let key_url = server.url("/aes/key.bin");
    let cipher_url = server.url("/aes/seg0.bin");
    let iv = aes128_iv();

    let key_bytes = key_store.get_raw_key(&key_url, Some(iv)).await?;
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
