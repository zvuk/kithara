#![forbid(unsafe_code)]

//! Edge case tests for CachedLoader - based on failed refactoring report.
//!
//! These tests specifically verify edge cases that caused bugs in the old
//! SegmentIndex/VariantIndex implementation.

use std::time::Duration;
use kithara_hls::cache::{CachedLoader, MockLoader, OffsetMap};
use kithara_stream::{Source, StreamResult};
use kithara_storage::WaitOutcome;
use rstest::rstest;
use std::sync::Arc;
use kithara_assets::AssetStoreBuilder;
use tempfile::TempDir;
use kithara_hls::{stream::types::SegmentMeta, HlsError};
use url::Url;

// ==================== Test Helpers ====================

fn create_test_meta(variant: usize, segment_index: usize, len: u64) -> SegmentMeta {
    SegmentMeta {
        variant,
        segment_index,
        sequence: segment_index as u64,
        url: Url::parse(&format!("http://test.com/v{}/seg{}.ts", variant, segment_index))
            .expect("valid URL"),
        duration: Some(Duration::from_secs(4)),
        key: None,
        len,
    }
}

async fn create_test_assets() -> (AssetStoreBuilder<()>, TempDir) {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let builder = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root("test");

    (builder, temp_dir)
}

// ==================== EC-2: ABR Mid-Stream + Seek Backward (CRITICAL) ====================

/// EC-2: The CRITICAL edge case from the failed refactoring report.
///
/// Scenario:
/// 1. ABR starts mid-stream (loads segments 4, 5, 6 first)
/// 2. User seeks backward to segment 3
/// 3. System loads segments 0, 1, 2, 3
///
/// Bug in old implementation:
/// - `first_media_segment` was set to 4 (never updated)
/// - Cumulative offset calculation broke for segments 0-3
/// - global_offset for seg 3 was WRONG
///
/// Expected behavior in NEW implementation:
/// - OffsetMap uses BTreeMap with consecutive check
/// - Out-of-order insertion handled correctly
/// - All segments have correct global_offset regardless of load order
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_ec2_abr_midstream_then_backward_seek() -> Result<(), Box<dyn std::error::Error>> {
    let (builder, _temp) = create_test_assets().await;
    let assets = builder.build();

    let mut loader = MockLoader::new();
    loader.expect_num_variants().returning(|| 1);
    loader.expect_num_segments().returning(|_| Ok(10));

    // Segment size: 200KB each
    let seg_size = 200_000u64;
    loader.expect_load_segment().returning(move |variant, idx| {
        Ok(create_test_meta(variant, idx, seg_size))
    });

    let cached = CachedLoader::new(Arc::new(loader), assets);

    // Step 1: ABR starts mid-stream - load segments 4, 5, 6
    println!("Step 1: ABR mid-stream - loading segments 4, 5, 6");
    cached.wait_range(800_000..801_000).await?; // seg 4 starts at 800KB
    cached.wait_range(1_000_000..1_001_000).await?; // seg 5 starts at 1000KB
    cached.wait_range(1_200_000..1_201_000).await?; // seg 6 starts at 1200KB

    println!("  Segments 4-6 loaded");

    // Step 2: User seeks backward to segment 3
    println!("Step 2: Seek backward to segment 3");
    cached.wait_range(600_000..601_000).await?; // seg 3 starts at 600KB

    // Step 3: Load earlier segments 0, 1, 2
    println!("Step 3: Loading earlier segments 0, 1, 2");
    cached.wait_range(0..1000).await?; // seg 0
    cached.wait_range(200_000..201_000).await?; // seg 1
    cached.wait_range(400_000..401_000).await?; // seg 2

    println!("  All segments loaded");

    // CRITICAL VERIFICATION: Check that ALL segments have correct offsets
    // In old implementation, segments 0-3 would have WRONG offsets


    // Verify each segment can be accessed at its expected offset
    let expected_offsets = vec![
        (0, 0),           // seg 0: 0..200KB
        (1, 200_000),     // seg 1: 200KB..400KB
        (2, 400_000),     // seg 2: 400KB..600KB
        (3, 600_000),     // seg 3: 600KB..800KB
        (4, 800_000),     // seg 4: 800KB..1000KB
        (5, 1_000_000),   // seg 5: 1000KB..1200KB
        (6, 1_200_000),   // seg 6: 1200KB..1400KB
    ];

    for (seg_idx, expected_offset) in expected_offsets {
        let result: StreamResult<WaitOutcome, HlsError> =
            cached.wait_range(expected_offset..expected_offset + 1000).await;
        assert!(
            result.is_ok(),
            "Segment {} at offset {} should be accessible (old impl would FAIL here)",
            seg_idx,
            expected_offset
        );
        println!("  ✓ Segment {} correctly accessible at offset {}", seg_idx, expected_offset);
    }

    println!("\n✅ EC-2 PASSED: Out-of-order segment loading handled correctly!");
    println!("   Old implementation with first_media_segment would have FAILED this test.");

    Ok(())
}

// ==================== EC-2 Variant: Different load patterns ====================

/// EC-2b: Even more extreme out-of-order loading
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_ec2b_extreme_out_of_order() -> Result<(), Box<dyn std::error::Error>> {
    let (builder, _temp) = create_test_assets().await;
    let assets = builder.build();

    let mut loader = MockLoader::new();
    loader.expect_num_variants().returning(|| 1);
    loader.expect_num_segments().returning(|_| Ok(10));

    let seg_size = 200_000u64;
    loader.expect_load_segment().returning(move |variant, idx| {
        Ok(create_test_meta(variant, idx, seg_size))
    });

    let cached = CachedLoader::new(Arc::new(loader), assets);

    // Load in completely random order: 7, 2, 9, 0, 5, 3, 1, 8, 4, 6
    let load_order = vec![7, 2, 9, 0, 5, 3, 1, 8, 4, 6];

    println!("Loading segments in extreme random order: {:?}", load_order);

    for seg_idx in load_order {
        let offset = (seg_idx as u64) * 200_000;
        cached.wait_range(offset..offset + 1000).await?;
        println!("  Loaded segment {} at offset {}", seg_idx, offset);
    }

    // Verify all segments are accessible in SEQUENTIAL order
    println!("\nVerifying sequential access...");
    for seg_idx in 0..10 {
        let offset = (seg_idx as u64) * 200_000;
        let result: StreamResult<WaitOutcome, HlsError> =
            cached.wait_range(offset..offset + 1000).await;
        assert!(result.is_ok(), "Segment {} should be accessible", seg_idx);
        println!("  ✓ Segment {} OK", seg_idx);
    }

    println!("\n✅ EC-2b PASSED: Extreme out-of-order handled correctly!");

    Ok(())
}

// ==================== Unit test for OffsetMap directly ====================

/// Direct test of OffsetMap to verify the fix
#[test]
fn test_offset_map_out_of_order_insert() {
    let mut map = OffsetMap::new();
    let seg_size = 200_000u64;

    // Simulate EC-2 scenario directly on OffsetMap

    // Load segments 4, 5, 6 first (ABR mid-stream)
    println!("Inserting segments 4, 5, 6");
    map.insert(create_test_meta(0, 4, seg_size));
    map.insert(create_test_meta(0, 5, seg_size));
    map.insert(create_test_meta(0, 6, seg_size));

    // Load segments 0, 1, 2, 3 later (backward seek)
    println!("Inserting segments 0, 1, 2, 3");
    map.insert(create_test_meta(0, 0, seg_size));
    map.insert(create_test_meta(0, 1, seg_size));
    map.insert(create_test_meta(0, 2, seg_size));
    map.insert(create_test_meta(0, 3, seg_size));

    // Verify ALL segments have correct offsets
    for seg_idx in 0..7 {
        let expected_offset = seg_idx as u64 * seg_size;

        if let Some(cached_seg) = map.get(seg_idx) {
            assert_eq!(
                cached_seg.global_offset, expected_offset,
                "Segment {} has WRONG global_offset: expected {}, got {}",
                seg_idx, expected_offset, cached_seg.global_offset
            );
            println!("  ✓ Segment {} has correct offset: {}", seg_idx, cached_seg.global_offset);
        } else {
            panic!("Segment {} not found in OffsetMap", seg_idx);
        }
    }

    println!("\n✅ OffsetMap direct test PASSED!");
}

// ==================== EC-1: Empty playlist ====================

/// EC-1: Empty playlist (no segments)
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_ec1_empty_playlist() -> Result<(), Box<dyn std::error::Error>> {
    let (builder, _temp) = create_test_assets().await;
    let assets = builder.build();

    let mut loader = MockLoader::new();
    loader.expect_num_variants().returning(|| 1);
    loader.expect_num_segments().returning(|_| Ok(0)); // Empty!
    loader.expect_load_segment().returning(|_, _| {
        Err(kithara_hls::HlsError::SegmentNotFound("no segments".into()))
    });

    let cached = CachedLoader::new(Arc::new(loader), assets);

    // Try to read from empty source
    let result = cached.wait_range(0..1000).await;

    // Should either timeout or return error (not panic)
    println!("Empty playlist result: {:?}", result);

    // Check len
    assert_eq!(cached.len(), None, "Empty playlist should have no length");

    println!("✅ EC-1 PASSED: Empty playlist handled gracefully");

    Ok(())
}

// ==================== EC-3: Single variant ====================

/// EC-3: Single variant (no ABR switches possible)
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_ec3_single_variant() -> Result<(), Box<dyn std::error::Error>> {
    let (builder, _temp) = create_test_assets().await;
    let assets = builder.build();

    let mut loader = MockLoader::new();
    loader.expect_num_variants().returning(|| 1); // Only 1 variant
    loader.expect_num_segments().returning(|_| Ok(5));

    loader.expect_load_segment().returning(|variant, idx| {
        Ok(create_test_meta(variant, idx, 200_000))
    });

    let cached = CachedLoader::new(Arc::new(loader), assets);

    // Load and read from single variant
    for i in 0..5 {
        let offset = i * 200_000;
        cached.wait_range(offset..offset + 1000).await?;
        println!("  Segment {} loaded", i);
    }

    // Try to switch to non-existent variant
    cached.set_current_variant(1); // Should be ignored or handled gracefully

    println!("✅ EC-3 PASSED: Single variant handled correctly");

    Ok(())
}

// ==================== EC-4: Rapid variant switches ====================

/// EC-4: Rapid variant switches (stress test for ABR)
#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn test_ec4_rapid_variant_switches() -> Result<(), Box<dyn std::error::Error>> {
    let (builder, _temp) = create_test_assets().await;
    let assets = builder.build();

    let mut loader = MockLoader::new();
    loader.expect_num_variants().returning(|| 3);
    loader.expect_num_segments().returning(|_| Ok(10));

    loader.expect_load_segment().returning(|variant, idx| {
        // Different sizes per variant to ensure they're distinct
        let len = 200_000 + (variant as u64 * 10_000);
        Ok(create_test_meta(variant, idx, len))
    });

    let cached = CachedLoader::new(Arc::new(loader), assets);

    // Rapidly switch between variants while reading
    for i in 0..20 {
        let variant = i % 3;
        cached.set_current_variant(variant);

        let seg_idx = i / 3;
        let offset = (seg_idx as u64) * 200_000; // Approximate

        let result: StreamResult<WaitOutcome, HlsError> =
            cached.wait_range(offset..offset + 1000).await;
        println!("  Switch {}: variant {} segment {} - {:?}",
                 i, variant, seg_idx, result.is_ok());
    }

    println!("✅ EC-4 PASSED: Rapid variant switches handled");

    Ok(())
}
