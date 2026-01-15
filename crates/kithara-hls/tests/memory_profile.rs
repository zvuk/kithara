//! Memory profiling tests for kithara-hls and its dependencies.
//!
//! Run with: cargo test -p kithara-hls --test memory_profile --features memprofile -- --nocapture
//!
//! For heap profiling with dhat, run the example:
//! cargo run -p kithara-hls --example hls_memprofile --features memprofile
//!
//! This will generate dhat-heap.json that can be viewed at https://nnethercote.github.io/dh_view/dh_view.html

use std::{sync::Arc, time::Duration};

use kithara_stream::Source;
use memory_stats::memory_stats;
use tempfile::TempDir;
use tokio::runtime::Runtime;

mod fixture;

use fixture::{AbrTestServer, TestServer, master_playlist};

fn print_memory(label: &str) {
    if let Some(usage) = memory_stats() {
        let rss_mb = usage.physical_mem as f64 / 1024.0 / 1024.0;
        let virtual_mb = usage.virtual_mem as f64 / 1024.0 / 1024.0;
        println!("{}: RSS={:.2}MB, Virtual={:.2}MB", label, rss_mb, virtual_mb);
    } else {
        println!("{}: Unable to get memory stats", label);
    }
}

/// Profile memory usage during HLS session creation and segment iteration.
#[test]
fn profile_hls_session_memory() {
    let rt = Runtime::new().expect("Failed to create runtime");

    rt.block_on(async {
        print_memory("Before test setup");

        // Create test server with multiple segments
        let server = TestServer::new().await;
        let master_url = server.url("/master.m3u8").expect("master url");

        print_memory("After server creation");

        // Create temp directory for cache
        let temp_dir = TempDir::new().expect("temp dir");

        let options = kithara_hls::HlsOptions {
            cache_dir: Some(temp_dir.path().to_path_buf()),
            ..Default::default()
        };

        print_memory("Before HlsSource::open");

        // Open HLS source
        let session = kithara_hls::HlsSource::open(master_url, options)
            .await
            .expect("HlsSource::open");

        print_memory("After HlsSource::open");

        let source = session.source();
        let source = Arc::new(source);

        print_memory("After session.source()");

        // Read some data using Source trait
        let mut total_bytes = 0usize;
        for offset in (0..100_000u64).step_by(4096) {
            match source.read_at(offset, 4096).await {
                Ok(bytes) => {
                    total_bytes += bytes.len();
                    if bytes.is_empty() {
                        break;
                    }
                    if total_bytes % 50_000 < 4096 {
                        print_memory(&format!("After reading {} bytes", total_bytes));
                    }
                }
                Err(e) => {
                    println!("Read error at offset {}: {:?}", offset, e);
                    break;
                }
            }
        }

        print_memory(&format!("After reading total {} bytes", total_bytes));

        // Drop source
        drop(source);
        print_memory("After dropping source");
    });

    print_memory("After runtime block_on");
}

/// Profile memory usage of assets layer using public API.
#[test]
fn profile_assets_layer_memory() {
    let rt = Runtime::new().expect("Failed to create runtime");

    rt.block_on(async {
        use kithara_assets::{AssetStoreBuilder, EvictConfig, ResourceKey};
        use kithara_storage::{Resource, StreamingResourceExt};
        use tokio_util::sync::CancellationToken;

        print_memory("Before assets test");

        let temp_dir = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();

        // Create asset store using builder (public API)
        let store = AssetStoreBuilder::new()
            .root_dir(temp_dir.path().to_path_buf())
            .asset_root("test_asset_root")
            .evict_config(EvictConfig::default())
            .cancel(cancel)
            .build();

        print_memory("After AssetStore creation");

        // Create multiple streaming resources
        let num_resources = 10;
        let chunk_size = 64 * 1024;
        let resource_size = 256 * 1024; // 256KB per resource

        for i in 0..num_resources {
            let key = ResourceKey::new(format!("segment_{}.bin", i));
            let resource = store.open_streaming_resource(&key).await.expect("open");

            // Write data
            let data = vec![i as u8; chunk_size];
            for offset in (0..resource_size).step_by(chunk_size) {
                resource.write_at(offset as u64, &data).await.expect("write");
            }
            resource.inner().commit(Some(resource_size as u64)).await.expect("commit");

            if i % 3 == 0 {
                print_memory(&format!("After creating resource {}", i));
            }
        }

        print_memory(&format!("After creating {} resources", num_resources));

        // Read from all resources
        for i in 0..num_resources {
            let key = ResourceKey::new(format!("segment_{}.bin", i));
            let resource = store.open_streaming_resource(&key).await.expect("open");

            let bytes = resource.read_at(0, chunk_size).await.expect("read");
            assert_eq!(bytes.len(), chunk_size);
        }

        print_memory("After reading from all resources");

        drop(store);
        print_memory("After dropping store");
    });
}

/// Profile full HLS playback workflow.
#[test]
fn profile_full_hls_workflow() {
    let rt = Runtime::new().expect("Failed to create runtime");

    rt.block_on(async {
        print_memory("Before workflow test");

        let server = TestServer::new().await;
        let temp_dir = TempDir::new().expect("temp dir");

        let options = kithara_hls::HlsOptions {
            cache_dir: Some(temp_dir.path().to_path_buf()),
            ..Default::default()
        };

        print_memory("Before opening session");

        // Open session
        let session = kithara_hls::HlsSource::open(
            server.url("/master.m3u8").expect("url"),
            options,
        )
        .await
        .expect("open");

        print_memory("After opening session");

        // Get events and source
        let _events = session.events();
        let source = Arc::new(session.source());

        print_memory("After getting source");

        // Simulate playback by reading chunks
        let chunk_size = 8 * 1024; // 8KB chunks
        let mut total_read = 0usize;
        let mut offset = 0u64;

        // Read up to 500KB
        while total_read < 500_000 {
            // Wait for data
            match source.wait_range(offset..offset + chunk_size as u64).await {
                Ok(kithara_storage::WaitOutcome::Ready) => {}
                Ok(kithara_storage::WaitOutcome::Eof) => {
                    println!("EOF at offset {}", offset);
                    break;
                }
                Err(e) => {
                    println!("Wait error: {:?}", e);
                    break;
                }
            }

            // Read data
            match source.read_at(offset, chunk_size).await {
                Ok(bytes) => {
                    if bytes.is_empty() {
                        break;
                    }
                    total_read += bytes.len();
                    offset += bytes.len() as u64;

                    if total_read % 100_000 < chunk_size {
                        print_memory(&format!("After reading {}KB", total_read / 1024));
                    }
                }
                Err(e) => {
                    println!("Read error: {:?}", e);
                    break;
                }
            }
        }

        print_memory(&format!("After reading {} bytes total", total_read));

        drop(source);
        print_memory("After dropping source");
    });

    print_memory("Final workflow");
}

/// Profile memory with multiple concurrent sessions.
#[test]
fn profile_concurrent_sessions() {
    let rt = Runtime::new().expect("Failed to create runtime");

    rt.block_on(async {
        print_memory("Before concurrent test");

        let server = TestServer::new().await;

        let mut sessions = Vec::new();

        // Create multiple sessions
        for i in 0..3 {
            let temp_dir = TempDir::new().expect("temp dir");

            let options = kithara_hls::HlsOptions {
                cache_dir: Some(temp_dir.path().to_path_buf()),
                ..Default::default()
            };

            let session = kithara_hls::HlsSource::open(
                server.url("/master.m3u8").expect("url"),
                options,
            )
            .await
            .expect("open");

            let source = Arc::new(session.source());
            sessions.push((source, temp_dir));

            print_memory(&format!("After creating session {}", i));
        }

        print_memory("After creating all sessions");

        // Read from each session
        for (i, (source, _)) in sessions.iter().enumerate() {
            let bytes = source.read_at(0, 4096).await.unwrap_or_default();
            println!("Session {} read {} bytes", i, bytes.len());
        }

        print_memory("After reading from all sessions");

        // Drop sessions one by one
        for (i, session) in sessions.into_iter().enumerate() {
            drop(session);
            print_memory(&format!("After dropping session {}", i));
        }

        print_memory("After dropping all sessions");
    });
}

/// Profile memory with large segments (200KB each) using AbrTestServer.
#[test]
fn profile_large_segments() {
    let rt = Runtime::new().expect("Failed to create runtime");

    rt.block_on(async {
        print_memory("Before large segment test");

        // Create server with large segments (200KB each via AbrTestServer)
        let server = AbrTestServer::new(
            master_playlist(256_000, 512_000, 1_024_000),
            false,
            Duration::from_millis(1),
        )
        .await;

        let temp_dir = TempDir::new().expect("temp dir");

        let options = kithara_hls::HlsOptions {
            cache_dir: Some(temp_dir.path().to_path_buf()),
            ..Default::default()
        };

        print_memory("Before HlsSource::open");

        let session = kithara_hls::HlsSource::open(
            server.url("/master.m3u8").expect("url"),
            options,
        )
        .await
        .expect("open");

        print_memory("After HlsSource::open");

        let source = Arc::new(session.source());

        print_memory("After session.source()");

        // Read larger amounts - 600KB (3 segments of 200KB each)
        let chunk_size = 16 * 1024; // 16KB chunks
        let mut total_read = 0usize;
        let mut offset = 0u64;
        let target_bytes = 600_000; // 600KB

        while total_read < target_bytes {
            match source.wait_range(offset..offset + chunk_size as u64).await {
                Ok(kithara_storage::WaitOutcome::Ready) => {}
                Ok(kithara_storage::WaitOutcome::Eof) => {
                    println!("EOF at offset {}", offset);
                    break;
                }
                Err(e) => {
                    println!("Wait error: {:?}", e);
                    break;
                }
            }

            match source.read_at(offset, chunk_size).await {
                Ok(bytes) => {
                    if bytes.is_empty() {
                        break;
                    }
                    total_read += bytes.len();
                    offset += bytes.len() as u64;

                    if total_read % 100_000 < chunk_size {
                        print_memory(&format!("After reading {}KB", total_read / 1024));
                    }
                }
                Err(e) => {
                    println!("Read error: {:?}", e);
                    break;
                }
            }
        }

        print_memory(&format!("After reading {} bytes total", total_read));

        drop(source);
        print_memory("After dropping source");
    });

    print_memory("Final large segment");
}

/// Profile memory growth pattern when reading sequentially.
#[test]
fn profile_memory_growth_pattern() {
    let rt = Runtime::new().expect("Failed to create runtime");

    rt.block_on(async {
        print_memory("=== START: Memory growth pattern test ===");

        let server = AbrTestServer::new(
            master_playlist(256_000, 512_000, 1_024_000),
            false,
            Duration::from_millis(1),
        )
        .await;

        let temp_dir = TempDir::new().expect("temp dir");

        let options = kithara_hls::HlsOptions {
            cache_dir: Some(temp_dir.path().to_path_buf()),
            ..Default::default()
        };

        print_memory("Before HlsSource::open");

        let session = kithara_hls::HlsSource::open(
            server.url("/master.m3u8").expect("url"),
            options,
        )
        .await
        .expect("open");

        print_memory("After HlsSource::open");

        let source = Arc::new(session.source());

        // Track memory at each 50KB read
        let chunk_size = 4 * 1024;
        let mut total_read = 0usize;
        let mut offset = 0u64;
        let mut last_report = 0usize;

        loop {
            match source.wait_range(offset..offset + chunk_size as u64).await {
                Ok(kithara_storage::WaitOutcome::Ready) => {}
                Ok(kithara_storage::WaitOutcome::Eof) => {
                    println!("EOF at offset {}", offset);
                    break;
                }
                Err(e) => {
                    println!("Wait error: {:?}", e);
                    break;
                }
            }

            match source.read_at(offset, chunk_size).await {
                Ok(bytes) => {
                    if bytes.is_empty() {
                        break;
                    }
                    total_read += bytes.len();
                    offset += bytes.len() as u64;

                    // Report every 50KB
                    if total_read - last_report >= 50_000 {
                        print_memory(&format!("Read {}KB", total_read / 1024));
                        last_report = total_read;
                    }
                }
                Err(e) => {
                    println!("Read error: {:?}", e);
                    break;
                }
            }
        }

        print_memory(&format!("=== FINAL: {} bytes total ===", total_read));

        drop(source);
        print_memory("After dropping source");
    });

    print_memory("After runtime");
}

/// Detailed breakdown by component.
#[test]
fn profile_component_breakdown() {
    let rt = Runtime::new().expect("Failed to create runtime");

    rt.block_on(async {
        use kithara_assets::{AssetStoreBuilder, EvictConfig};
        use kithara_net::{HttpClient, NetOptions};
        use tokio_util::sync::CancellationToken;

        println!("\n=== Component Memory Breakdown ===\n");

        print_memory("1. Baseline (tokio runtime)");

        // HttpClient (reqwest)
        let net = HttpClient::new(NetOptions::default());
        print_memory("2. + HttpClient (reqwest)");

        // TempDir
        let temp_dir = TempDir::new().expect("temp dir");
        print_memory("3. + TempDir");

        // AssetStore
        let cancel = CancellationToken::new();
        let store = AssetStoreBuilder::new()
            .root_dir(temp_dir.path().to_path_buf())
            .asset_root("test")
            .evict_config(EvictConfig::default())
            .cancel(cancel.clone())
            .build();
        print_memory("4. + AssetStore");

        // Test server (axum)
        let server = AbrTestServer::new(
            master_playlist(256_000, 512_000, 1_024_000),
            false,
            Duration::from_millis(1),
        )
        .await;
        print_memory("5. + TestServer (axum)");

        // HLS session
        let options = kithara_hls::HlsOptions {
            cache_dir: Some(temp_dir.path().to_path_buf()),
            ..Default::default()
        };
        let session = kithara_hls::HlsSource::open(
            server.url("/master.m3u8").expect("url"),
            options,
        )
        .await
        .expect("open");
        print_memory("6. + HlsSource::open");

        let source = Arc::new(session.source());
        print_memory("7. + session.source()");

        // Read data
        let mut total = 0usize;
        let mut offset = 0u64;
        while total < 200_000 {
            match source.wait_range(offset..offset + 4096).await {
                Ok(kithara_storage::WaitOutcome::Ready) => {}
                _ => break,
            }
            match source.read_at(offset, 4096).await {
                Ok(bytes) if !bytes.is_empty() => {
                    total += bytes.len();
                    offset += bytes.len() as u64;
                }
                _ => break,
            }
        }
        print_memory(&format!("8. + Read {}KB data", total / 1024));

        // Cleanup
        drop(source);
        drop(store);
        drop(net);
        print_memory("9. After cleanup");

        println!("\n=== End Breakdown ===\n");
    });
}

/// Profile isolated storage layer without HLS.
#[test]
fn profile_storage_layer_isolated() {
    let rt = Runtime::new().expect("Failed to create runtime");

    rt.block_on(async {
        use kithara_storage::{DiskOptions, StreamingResource, Resource, StreamingResourceExt};
        use tokio_util::sync::CancellationToken;

        print_memory("=== START: Storage layer isolated test ===");

        let temp_dir = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();

        // Create a single streaming resource and write/read large data
        let path = temp_dir.path().join("test_segment.bin");
        let opts = DiskOptions::new(&path, cancel);
        let resource = StreamingResource::open_disk(opts).await.expect("open");

        print_memory("After StreamingResource::open_disk");

        // Write 1MB in chunks
        let chunk_size = 64 * 1024;
        let total_size = 1024 * 1024;
        let data = vec![0xABu8; chunk_size];

        for offset in (0..total_size).step_by(chunk_size) {
            resource.write_at(offset as u64, &data).await.expect("write");
            if offset % (256 * 1024) == 0 {
                print_memory(&format!("After writing {}KB", offset / 1024));
            }
        }

        resource.commit(Some(total_size as u64)).await.expect("commit");
        print_memory("After commit");

        // Read back in chunks
        let mut total_read = 0usize;
        for offset in (0..total_size).step_by(chunk_size) {
            let bytes = resource.read_at(offset as u64, chunk_size).await.expect("read");
            total_read += bytes.len();
            if offset % (256 * 1024) == 0 {
                print_memory(&format!("After reading {}KB", offset / 1024));
            }
        }

        print_memory(&format!("=== FINAL: Read {} bytes ===", total_read));

        drop(resource);
        print_memory("After dropping resource");
    });

    print_memory("After runtime storage");
}
