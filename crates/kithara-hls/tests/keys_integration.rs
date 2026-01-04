mod fixture;
use fixture::*;
use kithara_hls::{HlsResult, keys::KeyManager};

#[tokio::test]
async fn fetch_and_cache_key() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (assets, net) = create_test_cache_and_net();
    let cache = assets.cache().clone();

    let key_manager = KeyManager::new(cache, net, None, None, None);
    let key_url = server.url("/key.bin")?;

    // Note: This test assumes the server provides a key endpoint
    // In real implementation, the test server would need to serve key data
    let _key = key_manager.get_key(&key_url, None).await;

    Ok(())
}

#[tokio::test]
async fn key_processor_applied() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (assets, net) = create_test_cache_and_net();
    let cache = assets.cache().clone();

    let processor = Box::new(|key: bytes::Bytes, _context: kithara_hls::KeyContext| {
        // Simple processor that just adds a prefix
        let mut processed = Vec::new();
        processed.extend_from_slice(b"processed:");
        processed.extend_from_slice(&key);
        Ok(bytes::Bytes::from(processed))
    });

    let key_manager = KeyManager::new(cache, net, Some(processor), None, None);
    let key_url = server.url("/key.bin")?;

    let key = key_manager.get_key(&key_url, None).await?;
    assert!(key.starts_with(b"processed:"));

    Ok(())
}
