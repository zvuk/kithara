//! Performance tests for storage operations.
//!
//! Run with: `cargo test --test storage --features perf --release`

#![cfg(feature = "perf")]

use std::io::Write;

use kithara::storage::{MmapOptions, MmapResource, OpenMode, Resource, ResourceExt};
use rstest::rstest;
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

/// Measure random read performance.
#[hotpath::measure]
fn storage_read_random(storage: &MmapResource, offset: u64) -> Vec<u8> {
    let mut buf = vec![0u8; 4096];
    storage.read_at(offset, &mut buf).unwrap();
    buf
}

/// Measure write performance.
#[hotpath::measure]
fn storage_write_chunk(storage: &MmapResource, offset: u64, data: &[u8]) {
    storage.write_at(offset, data).unwrap();
}

#[derive(Clone, Copy)]
enum PerfScenario {
    RandomReads,
    SequentialReads,
    WaitRange,
    Writes,
}

#[rstest]
#[case("storage_sequential_read", PerfScenario::SequentialReads)]
#[case("storage_random_read", PerfScenario::RandomReads)]
#[case("storage_write", PerfScenario::Writes)]
#[case("storage_wait_range", PerfScenario::WaitRange)]
#[test]
#[ignore]
fn perf_storage_scenarios(#[case] label: &'static str, #[case] scenario: PerfScenario) {
    let _guard = hotpath::FunctionsGuardBuilder::new(label).build();
    match scenario {
        PerfScenario::SequentialReads => {
            let (storage, _temp) = create_test_storage(10 * 1024 * 1024);
            let read_sizes = [4096, 16384, 65536, 262144];

            for &size in &read_sizes {
                for i in 0..10 {
                    let offset = (i * size) as u64;
                    let _ = storage_read_sequential(&storage, offset, size);
                }
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
        PerfScenario::RandomReads => {
            let (storage, _temp) = create_test_storage(100 * 1024 * 1024);
            let offsets: Vec<u64> = (0..1000)
                .map(|i| ((i * 7919) % 25000) as u64 * 4096)
                .collect();

            for &offset in offsets.iter().take(10) {
                let _ = storage_read_random(&storage, offset);
            }
            for &offset in &offsets {
                let _ = storage_read_random(&storage, offset);
            }

            println!("\n{:=<60}", "");
            println!("Random 4KB reads");
            println!("Iterations: 1000");
            println!("File size: 100MB");
            println!("{:=<60}\n", "");
        }
        PerfScenario::Writes => {
            let (storage, _temp) = create_test_storage(10 * 1024 * 1024);
            let chunk_sizes = [4096, 16384, 65536];

            for &size in &chunk_sizes {
                let data = vec![42u8; size];
                for i in 0..10 {
                    let offset = (i * size) as u64;
                    storage_write_chunk(&storage, offset, &data);
                }
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
        PerfScenario::WaitRange => {
            let (storage, _temp) = create_test_storage(10 * 1024 * 1024);
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
    }
}
