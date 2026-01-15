//! Heap profiling for kithara-assets.
//!
//! Run with: cargo run -p kithara-assets --example heap_profile
//!
//! This outputs dhat-heap.json which can be viewed at https://nnethercote.github.io/dh_view/dh_view.html

use std::sync::Arc;

use kithara_assets::{
    AssetStoreBuilder, Assets, CachedAssets, DiskAssetStore, EvictConfig, ResourceKey,
};
use kithara_storage::{Resource, StreamingResourceExt};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn print_rss(label: &str) {
    if let Some(usage) = memory_stats::memory_stats() {
        println!("[RSS] {}: {} KB", label, usage.physical_mem / 1024);
    }
}

#[tokio::main]
async fn main() {
    let _profiler = dhat::Profiler::new_heap();

    println!("=== Heap Profiling kithara-assets ===\n");
    print_rss("Initial");

    // Scenario 1: Create store and resources
    {
        println!("Scenario 1: Full AssetStore with 10 atomic + 10 streaming resources");
        let temp_dir = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();

        let store = AssetStoreBuilder::new()
            .root_dir(temp_dir.path())
            .asset_root("test-asset")
            .evict_config(EvictConfig {
                max_assets: Some(100),
                max_bytes: None,
            })
            .cancel(cancel.clone())
            .build();

        let stats_after_store = dhat::HeapStats::get();
        println!(
            "  After store creation: {} bytes in {} blocks",
            stats_after_store.curr_bytes, stats_after_store.curr_blocks
        );

        // Create atomic resources
        let mut atomic_handles = Vec::new();
        for i in 0..10 {
            let key = ResourceKey::new(format!("atomic/{i}.json"));
            let res = store.open_atomic_resource(&key).await.expect("open atomic");
            res.write(format!("data-{i}").as_bytes())
                .await
                .expect("write");
            atomic_handles.push(res);
        }

        let stats_after_atomic = dhat::HeapStats::get();
        println!(
            "  After 10 atomic:      {} bytes in {} blocks (+{} bytes)",
            stats_after_atomic.curr_bytes,
            stats_after_atomic.curr_blocks,
            stats_after_atomic.curr_bytes - stats_after_store.curr_bytes
        );

        // Create streaming resources
        let mut streaming_handles = Vec::new();
        for i in 0..10 {
            let key = ResourceKey::new(format!("streaming/{i}.bin"));
            let res = store
                .open_streaming_resource(&key)
                .await
                .expect("open streaming");
            res.write_at(0, &vec![0u8; 1024]).await.expect("write");
            streaming_handles.push(res);
        }

        let stats_after_streaming = dhat::HeapStats::get();
        println!(
            "  After 10 streaming:   {} bytes in {} blocks (+{} bytes)",
            stats_after_streaming.curr_bytes,
            stats_after_streaming.curr_blocks,
            stats_after_streaming.curr_bytes - stats_after_atomic.curr_bytes
        );
        print_rss("After 10 streaming resources");

        drop(streaming_handles);
        drop(atomic_handles);

        let stats_after_drop = dhat::HeapStats::get();
        println!(
            "  After dropping handles: {} bytes in {} blocks",
            stats_after_drop.curr_bytes, stats_after_drop.curr_blocks
        );
        print_rss("After dropping handles");
    }

    println!();

    // Scenario 2: Bare DiskAssetStore without decorators
    {
        println!("Scenario 2: Bare DiskAssetStore (no cache/evict/lease)");
        let temp_dir = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();

        let base = Arc::new(DiskAssetStore::new(
            temp_dir.path().to_path_buf(),
            "bare-asset".to_string(),
            cancel,
        ));

        let stats_before = dhat::HeapStats::get();

        let mut handles = Vec::new();
        for i in 0..10 {
            let key = ResourceKey::new(format!("res/{i}.bin"));
            let res = base.open_atomic_resource(&key).await.expect("open");
            handles.push(res);
        }

        let stats_after = dhat::HeapStats::get();
        println!(
            "  10 atomic resources: +{} bytes in +{} blocks",
            stats_after.curr_bytes - stats_before.curr_bytes,
            stats_after.curr_blocks - stats_before.curr_blocks
        );
        println!(
            "  Per resource: ~{} bytes",
            (stats_after.curr_bytes - stats_before.curr_bytes) / 10
        );

        drop(handles);
    }

    println!();

    // Scenario 3: CachedAssets layer overhead
    {
        println!("Scenario 3: CachedAssets layer overhead");
        let temp_dir = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();

        let cached = CachedAssets::new(Arc::new(DiskAssetStore::new(
            temp_dir.path().to_path_buf(),
            "cached-asset".to_string(),
            cancel,
        )));

        let stats_before = dhat::HeapStats::get();

        let mut handles = Vec::new();
        for i in 0..10 {
            let key = ResourceKey::new(format!("res/{i}.bin"));
            let res = cached.open_atomic_resource(&key).await.expect("open");
            handles.push(res);
        }

        let stats_after = dhat::HeapStats::get();
        println!(
            "  10 cached atomic resources: +{} bytes in +{} blocks",
            stats_after.curr_bytes - stats_before.curr_bytes,
            stats_after.curr_blocks - stats_before.curr_blocks
        );
        println!(
            "  Per cached resource: ~{} bytes",
            (stats_after.curr_bytes - stats_before.curr_bytes) / 10
        );

        drop(handles);
    }

    println!("\n=== Profile saved to dhat-heap.json ===");
    println!("View at: https://nnethercote.github.io/dh_view/dh_view.html");
}
