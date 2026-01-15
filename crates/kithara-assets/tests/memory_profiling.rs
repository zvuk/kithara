//! Memory profiling tests for kithara-assets.
//!
//! Run with: cargo test -p kithara-assets --test memory_profiling -- --nocapture

use std::sync::Arc;

use kithara_assets::{
    AssetStoreBuilder, Assets, CachedAssets, DiskAssetStore, EvictAssets, EvictConfig,
    LeaseAssets, LeaseGuard, ResourceKey,
};
use kithara_storage::{AtomicResource, Resource, StreamingResource, StreamingResourceExt};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn size_report<T>(name: &str) {
    println!("  {:<45} {:>6} bytes", name, std::mem::size_of::<T>());
}

#[test]
fn print_struct_sizes() {
    println!("\n=== Struct Sizes (compile-time) ===\n");

    println!("Core types:");
    size_report::<ResourceKey>("ResourceKey");
    size_report::<AtomicResource>("AtomicResource");
    size_report::<StreamingResource>("StreamingResource");

    println!("\nBase store:");
    size_report::<DiskAssetStore>("DiskAssetStore");
    size_report::<Arc<DiskAssetStore>>("Arc<DiskAssetStore>");

    println!("\nDecorators:");
    size_report::<EvictAssets<DiskAssetStore>>("EvictAssets<DiskAssetStore>");
    size_report::<CachedAssets<EvictAssets<DiskAssetStore>>>("CachedAssets<EvictAssets<...>>");
    size_report::<LeaseAssets<CachedAssets<EvictAssets<DiskAssetStore>>>>(
        "LeaseAssets<CachedAssets<EvictAssets<...>>>",
    );

    println!("\nLeaseGuard (per-resource overhead):");
    size_report::<LeaseGuard<CachedAssets<EvictAssets<DiskAssetStore>>>>(
        "LeaseGuard<CachedAssets<EvictAssets<...>>>",
    );

    println!("\nDashMap internals (approximate):");
    size_report::<dashmap::DashMap<String, String>>("DashMap<String, String>");
    size_report::<dashmap::DashMap<ResourceKey, AtomicResource>>(
        "DashMap<ResourceKey, AtomicResource>",
    );

    println!("\nCancellationToken:");
    size_report::<CancellationToken>("CancellationToken");
    size_report::<tokio::sync::Mutex<std::collections::HashSet<String>>>(
        "tokio::sync::Mutex<HashSet<String>>",
    );

    println!();
}

#[tokio::test]
async fn measure_memory_with_resources() {
    println!("\n=== Runtime Memory Measurement ===\n");

    let initial_mem = memory_stats::memory_stats().map(|s| s.physical_mem);

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

    let after_store = memory_stats::memory_stats().map(|s| s.physical_mem);

    // Create multiple atomic resources
    let mut atomic_handles = Vec::new();
    for i in 0..10 {
        let key = ResourceKey::new(format!("atomic/{i}.json"));
        let res = store.open_atomic_resource(&key).await.expect("open atomic");
        res.write(format!("data-{i}").as_bytes())
            .await
            .expect("write");
        atomic_handles.push(res);
    }

    let after_atomic = memory_stats::memory_stats().map(|s| s.physical_mem);

    // Create multiple streaming resources
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

    let after_streaming = memory_stats::memory_stats().map(|s| s.physical_mem);

    // Report
    if let (Some(initial), Some(after_s), Some(after_a), Some(after_str)) =
        (initial_mem, after_store, after_atomic, after_streaming)
    {
        println!("Memory usage (RSS):");
        println!("  Initial:              {:>10} bytes", initial);
        println!(
            "  After store creation: {:>10} bytes (+{} bytes)",
            after_s,
            after_s.saturating_sub(initial)
        );
        println!(
            "  After 10 atomic res:  {:>10} bytes (+{} bytes)",
            after_a,
            after_a.saturating_sub(after_s)
        );
        println!(
            "  After 10 streaming:   {:>10} bytes (+{} bytes)",
            after_str,
            after_str.saturating_sub(after_a)
        );
        println!(
            "\n  Total overhead:       {:>10} bytes",
            after_str.saturating_sub(initial)
        );
    } else {
        println!("  (memory_stats not available on this platform)");
    }

    // Keep handles alive for measurement
    drop(streaming_handles);
    drop(atomic_handles);

    println!();
}

#[tokio::test]
async fn measure_cache_overhead() {
    println!("\n=== Cache Layer Overhead ===\n");

    let temp_dir = TempDir::new().expect("temp dir");
    let cancel = CancellationToken::new();

    // Without cache layer
    let base_store = Arc::new(DiskAssetStore::new(
        temp_dir.path().to_path_buf(),
        "nocache-asset".to_string(),
        cancel.clone(),
    ));

    let before_nocache = memory_stats::memory_stats().map(|s| s.physical_mem);

    let mut handles_nocache = Vec::new();
    for i in 0..20 {
        let key = ResourceKey::new(format!("res/{i}.bin"));
        let res = base_store.open_atomic_resource(&key).await.expect("open");
        handles_nocache.push(res);
    }

    let after_nocache = memory_stats::memory_stats().map(|s| s.physical_mem);

    // With cache layer
    let cached_store = CachedAssets::new(Arc::new(DiskAssetStore::new(
        temp_dir.path().to_path_buf(),
        "cached-asset".to_string(),
        cancel.clone(),
    )));

    let before_cached = memory_stats::memory_stats().map(|s| s.physical_mem);

    let mut handles_cached = Vec::new();
    for i in 0..20 {
        let key = ResourceKey::new(format!("res/{i}.bin"));
        let res = cached_store.open_atomic_resource(&key).await.expect("open");
        handles_cached.push(res);
    }

    let after_cached = memory_stats::memory_stats().map(|s| s.physical_mem);

    if let (Some(b_nc), Some(a_nc), Some(b_c), Some(a_c)) =
        (before_nocache, after_nocache, before_cached, after_cached)
    {
        let nocache_delta = a_nc.saturating_sub(b_nc);
        let cached_delta = a_c.saturating_sub(b_c);

        println!("20 atomic resources:");
        println!("  Without CachedAssets: +{:>8} bytes", nocache_delta);
        println!("  With CachedAssets:    +{:>8} bytes", cached_delta);
        println!(
            "  Cache overhead:       +{:>8} bytes ({:.1}%)",
            cached_delta.saturating_sub(nocache_delta),
            if nocache_delta > 0 {
                (cached_delta.saturating_sub(nocache_delta) as f64 / nocache_delta as f64) * 100.0
            } else {
                0.0
            }
        );
    }

    drop(handles_cached);
    drop(handles_nocache);

    println!();
}

#[tokio::test]
async fn measure_dashmap_entry_overhead() {
    println!("\n=== DashMap Entry Overhead ===\n");

    let map: dashmap::DashMap<ResourceKey, AtomicResource> = dashmap::DashMap::new();

    let before = memory_stats::memory_stats().map(|s| s.physical_mem);

    let temp_dir = TempDir::new().expect("temp dir");
    let cancel = CancellationToken::new();
    let base = Arc::new(DiskAssetStore::new(
        temp_dir.path().to_path_buf(),
        "map-test".to_string(),
        cancel,
    ));

    // Insert 100 entries
    for i in 0..100 {
        let key = ResourceKey::new(format!("entry/{i}.json"));
        let res = base.open_atomic_resource(&key).await.expect("open");
        map.insert(key, res);
    }

    let after = memory_stats::memory_stats().map(|s| s.physical_mem);

    if let (Some(b), Some(a)) = (before, after) {
        let total = a.saturating_sub(b);
        println!("100 DashMap entries:");
        println!("  Total memory:     {:>10} bytes", total);
        println!("  Per entry:        {:>10} bytes", total / 100);
    }

    println!("  DashMap capacity: {:>10}", map.capacity());
    println!("  DashMap len:      {:>10}", map.len());

    println!();
}
