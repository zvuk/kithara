//! Memory profiling example for HLS playback.
//!
//! Run with: cargo run -p kithara-hls --example hls_memprofile --features memprofile -- [URL]
//!
//! This will:
//! 1. Print RSS memory usage at key points
//! 2. Generate dhat-heap.json with heap profiling data
//!
//! View dhat-heap.json at: https://nnethercote.github.io/dh_view/dh_view.html

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use std::{env::args, error::Error, sync::Arc, time::Duration};

use kithara_assets::EvictConfig;
use kithara_hls::{HlsOptions, HlsSource};
use kithara_stream::Source;
use memory_stats::memory_stats;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::EnvFilter;
use url::Url;

fn print_memory(label: &str) {
    if let Some(usage) = memory_stats() {
        let rss_mb = usage.physical_mem as f64 / 1024.0 / 1024.0;
        let virtual_mb = usage.virtual_mem as f64 / 1024.0 / 1024.0;
        println!("[MEMORY] {}: RSS={:.2}MB, Virtual={:.2}MB", label, rss_mb, virtual_mb);
    } else {
        println!("[MEMORY] {}: Unable to get memory stats", label);
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    // Initialize dhat profiler
    let _profiler = dhat::Profiler::new_heap();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_hls=info".parse()?)
                .add_directive("kithara_stream=info".parse()?)
                .add_directive("kithara_net=info".parse()?)
                .add_directive("kithara_storage=info".parse()?)
                .add_directive("kithara_assets=info".parse()?)
                .add_directive(LevelFilter::INFO.into()),
        )
        .with_line_number(true)
        .with_file(true)
        .init();

    print_memory("Initial");

    let url = args()
        .nth(1)
        .unwrap_or_else(|| "https://stream.silvercomet.top/hls/master.m3u8".to_string());

    let url: Url = url.parse()?;
    let temp_dir = TempDir::new()?;
    let cancel = CancellationToken::new();

    print_memory("After URL parsing");

    let hls_options = HlsOptions {
        cache_dir: Some(temp_dir.path().to_path_buf()),
        evict_config: Some(EvictConfig::default()),
        cancel: Some(cancel.clone()),
        ..Default::default()
    };

    print_memory("Before HlsSource::open");

    // Open HLS source
    let session = HlsSource::open(url, hls_options).await?;

    print_memory("After HlsSource::open");

    let events_rx = session.events();
    let source = Arc::new(session.source());

    print_memory("After session.source()");

    // Spawn event handler
    let events_handle = tokio::spawn(async move {
        let mut events_rx = events_rx;
        while let Ok(ev) = events_rx.recv().await {
            info!(?ev, "Stream event");
        }
    });

    // Read data in chunks and measure memory
    let chunk_size = 16 * 1024; // 16KB chunks
    let max_bytes = 5 * 1024 * 1024; // Read up to 5MB
    let mut total_bytes = 0u64;
    let mut offset = 0u64;
    let mut buf = vec![0u8; chunk_size]; // Reusable buffer

    println!("\n[PROFILE] Starting data read (up to {}MB)...", max_bytes / 1024 / 1024);

    while total_bytes < max_bytes as u64 {
        // Wait for data to be available
        match source.wait_range(offset..offset + chunk_size as u64).await {
            Ok(kithara_storage::WaitOutcome::Ready) => {}
            Ok(kithara_storage::WaitOutcome::Eof) => {
                println!("[PROFILE] EOF reached at offset {}", offset);
                break;
            }
            Err(e) => {
                println!("[PROFILE] Wait error: {:?}", e);
                break;
            }
        }

        // Read data
        match source.read_at(offset, &mut buf).await {
            Ok(len) => {
                if len == 0 {
                    break;
                }
                total_bytes += len as u64;
                offset += len as u64;

                // Print memory every 500KB
                if total_bytes % (500 * 1024) < chunk_size as u64 {
                    print_memory(&format!("After reading {}KB", total_bytes / 1024));
                }
            }
            Err(e) => {
                println!("[PROFILE] Read error: {:?}", e);
                break;
            }
        }

        // Small delay to allow async tasks to process
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    println!("\n[PROFILE] Read {} bytes total", total_bytes);
    print_memory("After all reads");

    // Clean up
    cancel.cancel();
    events_handle.abort();

    drop(source);
    print_memory("After dropping source");

    // Give dhat time to finalize
    tokio::time::sleep(Duration::from_millis(100)).await;

    print_memory("Final");

    println!("\n[PROFILE] dhat heap profile saved to dhat-heap.json");
    println!("[PROFILE] View at: https://nnethercote.github.io/dh_view/dh_view.html");

    Ok(())
}
