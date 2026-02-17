//! Performance tests for storage operations.
//!
//! Run with: `cargo test --test storage --features perf --release`

#![cfg(feature = "perf")]

use std::io::Write;

use kithara::storage::{MmapOptions, MmapResource, OpenMode, Resource, ResourceExt};
use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

/// Helper to create test storage with data.
fn create_test_storage(size: usize) -> (MmapResource, NamedTempFile) {
    let mut file = NamedTempFile::new().unwrap();
    let data = vec![0u8; size];
    file.write_all(&data).unwrap();
    file.flush().unwrap();

    let path = file.path().to_path_buf();
    let opts = MmapOptions {
        path,
        initial_len: None,
        mode: OpenMode::ReadWrite,
    };
    let storage: MmapResource = Resource::open(CancellationToken::new(), opts).unwrap();
    (storage, file)
}

/// Measure sequential read performance.
#[hotpath::measure]
fn storage_read_sequential(storage: &MmapResource, offset: u64, size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size];
    storage.read_at(offset, &mut buf).unwrap();
    buf
}

#[test]
#[ignore]
fn perf_storage_sequential_reads() {
    let _guard = hotpath::FunctionsGuardBuilder::new("storage_sequential_read").build();

    // 10MB file
    let (storage, _temp) = create_test_storage(10 * 1024 * 1024);

    // Different read sizes
    let read_sizes = [4096, 16384, 65536, 262144]; // 4KB, 16KB, 64KB, 256KB

    for &size in &read_sizes {
        // Warm-up
        for i in 0..10 {
            let offset = (i * size) as u64;
            let _ = storage_read_sequential(&storage, offset, size);
        }

        // Measure 1000 sequential reads
        for i in 0..1000 {
            let offset = (i * size) as u64;
            let _ = storage_read_sequential(&storage, offset, size);
        }

        println!("\n{:=<60}", "");
        println!("Sequential reads: {} bytes", size);
        println!("Iterations: 1000");
        println!("{:=<60}\n", "");
    }
}

/// Measure random read performance.
#[hotpath::measure]
fn storage_read_random(storage: &MmapResource, offset: u64) -> Vec<u8> {
    let mut buf = vec![0u8; 4096];
    storage.read_at(offset, &mut buf).unwrap();
    buf
}

#[test]
#[ignore]
fn perf_storage_random_reads() {
    let _guard = hotpath::FunctionsGuardBuilder::new("storage_random_read").build();

    // 100MB file for random access
    let (storage, _temp) = create_test_storage(100 * 1024 * 1024);

    // Generate pseudo-random offsets
    let offsets: Vec<u64> = (0..1000)
        .map(|i| ((i * 7919) % 25000) as u64 * 4096) // pseudo-random within 100MB
        .collect();

    // Warm-up
    for &offset in offsets.iter().take(10) {
        let _ = storage_read_random(&storage, offset);
    }

    // Measure 1000 random reads
    for &offset in &offsets {
        let _ = storage_read_random(&storage, offset);
    }

    println!("\n{:=<60}", "");
    println!("Random 4KB reads");
    println!("Iterations: 1000");
    println!("File size: 100MB");
    println!("{:=<60}\n", "");
}

/// Measure write performance.
#[hotpath::measure]
fn storage_write_chunk(storage: &MmapResource, offset: u64, data: &[u8]) {
    storage.write_at(offset, data).unwrap();
}

#[test]
#[ignore]
fn perf_storage_writes() {
    let _guard = hotpath::FunctionsGuardBuilder::new("storage_write").build();

    // Create empty 10MB file
    let (storage, _temp) = create_test_storage(10 * 1024 * 1024);

    let chunk_sizes = [4096, 16384, 65536]; // 4KB, 16KB, 64KB

    for &size in &chunk_sizes {
        let data = vec![42u8; size];

        // Warm-up
        for i in 0..10 {
            let offset = (i * size) as u64;
            storage_write_chunk(&storage, offset, &data);
        }

        // Measure 100 writes (less than reads due to disk wear)
        for i in 0..100 {
            let offset = (i * size) as u64;
            storage_write_chunk(&storage, offset, &data);
        }

        println!("\n{:=<60}", "");
        println!("Sequential writes: {} bytes", size);
        println!("Iterations: 100");
        println!("{:=<60}\n", "");
    }
}

/// Measure wait_range overhead (availability check).
#[test]
#[ignore]
fn perf_storage_wait_range() {
    let _guard = hotpath::FunctionsGuardBuilder::new("storage_wait_range").build();

    let (storage, _temp) = create_test_storage(10 * 1024 * 1024);

    // Measure wait_range for already-available data
    hotpath::measure_block!("wait_range_available", {
        for i in 0..10000 {
            let offset = (i % 1000) * 4096;
            let _ = storage.wait_range(offset..offset + 4096);
        }
    });

    println!("\n{:=<60}", "");
    println!("wait_range for available data");
    println!("Iterations: 10000");
    println!("{:=<60}\n", "");
}
