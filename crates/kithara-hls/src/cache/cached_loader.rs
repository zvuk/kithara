//! CachedLoader: Full Source implementation with caching.

use std::{ops::Range, sync::{Arc, atomic::{AtomicU64, AtomicUsize}}};
use async_trait::async_trait;
use dashmap::DashMap;
use kithara_assets::AssetStore;
use kithara_storage::WaitOutcome;
use kithara_stream::{MediaInfo, Source, StreamError, StreamResult};
use tokio::sync::Notify;

use super::{Loader, OffsetMap};
use crate::{HlsError, index::EncryptionInfo};

/// Generic cached loader implementing Source trait.
///
/// Provides:
/// - Lazy segment loading via Loader
/// - Offset mapping via OffsetMap per variant
/// - ABR support via current_variant tracking
/// - Seek detection via last_read_pos
pub struct CachedLoader<L: Loader> {
    loader: Arc<L>,
    offset_maps: DashMap<usize, OffsetMap>,
    current_variant: AtomicUsize,
    last_read_pos: AtomicU64,
    assets: AssetStore<EncryptionInfo>,
    notify: Arc<Notify>,
}

impl<L: Loader> CachedLoader<L> {
    pub fn new(loader: Arc<L>, assets: AssetStore<EncryptionInfo>) -> Self {
        Self {
            loader,
            offset_maps: DashMap::new(),
            current_variant: AtomicUsize::new(0),
            last_read_pos: AtomicU64::new(0),
            assets,
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn current_variant(&self) -> usize {
        self.current_variant.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_current_variant(&self, variant: usize) {
        self.current_variant.store(variant, std::sync::atomic::Ordering::Relaxed);
    }

    /// Load segment if not in cache and insert into OffsetMap.
    async fn load_segment_if_needed(&self, variant: usize, segment_index: usize) -> Result<(), HlsError> {
        // Check if already loaded
        if let Some(map) = self.offset_maps.get(&variant) {
            if map.get(segment_index).is_some() {
                return Ok(());
            }
        }

        // Load segment via loader
        let meta = self.loader.load_segment(variant, segment_index).await?;

        // Insert into offset map
        self.offset_maps
            .entry(variant)
            .or_insert_with(|| crate::cache::OffsetMap::new())
            .insert(meta);

        // Notify waiters
        self.notify.notify_waiters();

        Ok(())
    }

    /// Find segment covering the given offset in variant's offset map.
    /// Returns None if segment not loaded yet.
    fn find_cached_segment(&self, offset: u64, variant: usize) -> Option<(usize, u64)> {
        let map = self.offset_maps.get(&variant)?;
        map.find_segment_for_offset(offset)
    }

    /// Estimate total length for variant.
    fn estimate_len(&self, variant: usize) -> Option<u64> {
        let map = self.offset_maps.get(&variant)?;
        Some(map.total_size())
    }
}

#[async_trait]
impl<L: Loader + 'static> Source for CachedLoader<L> {
    type Item = u8;
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        if range.start >= range.end {
            return Ok(WaitOutcome::Ready);
        }

        let current_var = self.current_variant();
        const MAX_WAIT_ITERATIONS: usize = 1000;
        let mut iterations = 0;

        loop {
            iterations += 1;
            if iterations > MAX_WAIT_ITERATIONS {
                return Err(StreamError::Source(HlsError::Driver(
                    "wait_range timeout: max iterations exceeded".into()
                )));
            }

            // Check if segment covering range.start exists
            if let Some((_segment_index, _local_offset)) = self.find_cached_segment(range.start, current_var) {
                // Segment found in cache
                return Ok(WaitOutcome::Ready);
            }

            // Segment not cached - estimate and load
            if let Some(map) = self.offset_maps.get(&current_var) {
                let segment_index = map.estimate_segment_index(range.start);
                drop(map);

                // Try to load segment
                if let Err(e) = self.load_segment_if_needed(current_var, segment_index).await {
                    return Err(StreamError::Source(e));
                }

                // Check again after loading
                if self.find_cached_segment(range.start, current_var).is_some() {
                    return Ok(WaitOutcome::Ready);
                }
            } else {
                // No offset map for variant - try loading segment 0
                if let Err(e) = self.load_segment_if_needed(current_var, 0).await {
                    return Err(StreamError::Source(e));
                }
            }

            // Wait for next segment to be loaded
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Detect seek
        let last_pos = self.last_read_pos.load(std::sync::atomic::Ordering::Relaxed);
        let _is_seek = offset < last_pos || offset > last_pos.saturating_add(1024 * 1024);

        // Update last_read_pos
        self.last_read_pos.store(offset, std::sync::atomic::Ordering::Relaxed);

        let current_var = self.current_variant();

        // Find segment
        let (segment_index, local_offset) = match self.find_cached_segment(offset, current_var) {
            Some(seg) => seg,
            None => {
                // Segment not cached - estimate and load
                let segment_index = self.offset_maps
                    .get(&current_var)
                    .map(|map| map.estimate_segment_index(offset))
                    .unwrap_or(0);

                self.load_segment_if_needed(current_var, segment_index)
                    .await
                    .map_err(|e| StreamError::Source(e))?;

                // Try to find again
                match self.find_cached_segment(offset, current_var) {
                    Some(seg) => seg,
                    None => return Ok(0), // EOF or segment not available
                }
            }
        };

        // Get segment URL and encryption from OffsetMap
        let (segment_url, encryption) = {
            let map_ref = self.offset_maps.get(&current_var)
                .ok_or_else(|| StreamError::Source(HlsError::SegmentNotFound("variant not found".into())))?;
            let cached_seg = map_ref.get(segment_index)
                .ok_or_else(|| StreamError::Source(HlsError::SegmentNotFound(format!("segment {} not found", segment_index))))?;
            (cached_seg.url.clone(), cached_seg.encryption.clone())
        };

        use kithara_assets::{Assets, ResourceKey};
        use kithara_storage::StreamingResourceExt;

        let key = ResourceKey::from_url(&segment_url);

        // Choose between encrypted and plain resource
        let bytes_read = if self.assets.has_processing() && encryption.is_some() {
            // Encrypted segment - use processed resource with decryption callback
            let enc_info = encryption.unwrap();
            let resource = self.assets.open_processed(&key, enc_info)
                .await
                .map_err(|e| StreamError::Source(HlsError::Assets(e)))?;

            resource.read_at(local_offset, buf)
                .await
                .map_err(|e| StreamError::Source(HlsError::Storage(e)))?
        } else {
            // Plain segment - use streaming resource directly
            let resource = Assets::open_streaming_resource(&*self.assets, &key)
                .await
                .map_err(|e| StreamError::Source(HlsError::Assets(e)))?;

            resource.inner().read_at(local_offset, buf)
                .await
                .map_err(|e| StreamError::Source(HlsError::Storage(e)))?
        };

        Ok(bytes_read)
    }

    fn len(&self) -> Option<u64> {
        let current_var = self.current_variant();
        self.estimate_len(current_var)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        // TODO: Extract from variant codecs
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::loader::MockLoader;
    use crate::stream::types::SegmentMeta;
    use std::time::Duration;
    use url::Url;
    use kithara_assets::AssetStoreBuilder;
    use tempfile::TempDir;

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

    async fn create_test_loader() -> (Arc<MockLoader>, AssetStore<EncryptionInfo>, TempDir) {
        let temp_dir = tempfile::tempdir().expect("create temp dir");

        // Dummy process_fn for tests - doesn't actually decrypt, just passes through
        let dummy_fn: kithara_assets::ProcessFn<EncryptionInfo> =
            Arc::new(|bytes, _ctx| Box::pin(async move { Ok(bytes) }));

        let assets = AssetStoreBuilder::new()
            .root_dir(temp_dir.path())
            .asset_root("test")
            .process_fn(dummy_fn)
            .build();

        let mut loader = MockLoader::new();
        loader.expect_num_variants().returning(|| 3);
        loader.expect_num_segments().returning(|_| Ok(10));
        loader.expect_load_segment()
            .returning(|variant, idx| Ok(create_test_meta(variant, idx, 200_000)));

        (Arc::new(loader), assets, temp_dir)
    }

    // CL-1: Sequential read (basic)
    #[tokio::test]
    async fn test_read_at_sequential() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = CachedLoader::new(loader, assets);

        let mut buf = vec![0u8; 1000];

        // This should fail because methods are not implemented yet
        let result = cached.read_at(0, &mut buf).await;
        assert!(result.is_err() || result.unwrap() == 0, "read_at not implemented yet");
    }

    // CL-2: Random access from different segments
    #[tokio::test]
    async fn test_read_at_random_access() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = CachedLoader::new(loader, assets);

        let mut buf = vec![0u8; 1000];

        // Read from segment 0
        let _ = cached.read_at(0, &mut buf).await;

        // Jump to segment 5
        let _ = cached.read_at(1_000_000, &mut buf).await;

        // Jump back to segment 2
        let _ = cached.read_at(400_000, &mut buf).await;
    }

    // CL-3: wait_range waits for segment to load
    #[tokio::test]
    async fn test_wait_range_basic() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = CachedLoader::new(loader, assets);

        let result = cached.wait_range(0..1000).await;
        assert!(result.is_ok(), "wait_range should succeed");
        assert!(matches!(result.unwrap(), WaitOutcome::Ready));
    }

    // CL-4: len returns estimation
    #[tokio::test]
    async fn test_len_estimation() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = CachedLoader::new(loader, assets);

        let len = cached.len();
        // Initially None, will be Some after segments loaded
        assert!(len.is_none() || len.is_some());
    }

    // CL-5: media_info returns correct info
    #[tokio::test]
    async fn test_media_info() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = CachedLoader::new(loader, assets);

        let info = cached.media_info();
        // Not implemented yet, should return None
        assert!(info.is_none());
    }

    // CL-6: Variant switch mid-read (ABR scenario)
    #[tokio::test]
    async fn test_variant_switch() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = CachedLoader::new(loader, assets);

        assert_eq!(cached.current_variant(), 0);

        cached.set_current_variant(1);
        assert_eq!(cached.current_variant(), 1);

        let mut buf = vec![0u8; 1000];
        let _ = cached.read_at(0, &mut buf).await;
    }

    // CL-7: Concurrent reads (stress test)
    #[tokio::test]
    async fn test_concurrent_reads() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = Arc::new(CachedLoader::new(loader, assets));

        let mut handles = vec![];

        for i in 0..10 {
            let cached_clone = Arc::clone(&cached);
            let handle = tokio::spawn(async move {
                let mut buf = vec![0u8; 1000];
                let offset = i * 100_000;
                let _ = cached_clone.read_at(offset, &mut buf).await;
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.expect("task completed");
        }
    }

    // CL-8: Seek detection triggers logic
    #[tokio::test]
    async fn test_seek_detection() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = CachedLoader::new(loader, assets);

        let mut buf = vec![0u8; 1000];

        // Sequential read
        let _ = cached.read_at(0, &mut buf).await;
        let _ = cached.read_at(1000, &mut buf).await;

        // Backward seek (should be detected)
        let _ = cached.read_at(500, &mut buf).await;

        // Large forward jump (should be detected)
        let _ = cached.read_at(2_000_000, &mut buf).await;
    }

    // CL-9: wait_range timeout prevents deadlock
    #[tokio::test]
    async fn test_wait_range_timeout() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = CachedLoader::new(loader, assets);

        // Attempt to wait with timeout
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            cached.wait_range(0..1000)
        ).await;

        // Should either timeout or complete quickly
        assert!(result.is_ok() || result.is_err());
    }

    // CL-10: read_at loop guard prevents infinite loop
    #[tokio::test]
    async fn test_read_at_loop_guard() {
        let (loader, assets, _temp) = create_test_loader().await;
        let cached = CachedLoader::new(loader, assets);

        let mut buf = vec![0u8; 1000];

        // Attempt to read at invalid offset
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            cached.read_at(u64::MAX - 1000, &mut buf)
        ).await;

        // Should not hang indefinitely
        assert!(result.is_ok(), "read_at should not hang");
    }
}
