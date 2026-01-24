//! Generic segment loader trait.

use async_trait::async_trait;

use super::types::SegmentMeta;
use crate::HlsResult;

/// Generic segment loader.
///
/// Implementations:
/// - `FetchLoader`: production loader using FetchManager
/// - `MockLoader`: test mock via mockall
#[mockall::automock]
#[async_trait]
pub trait Loader: Send + Sync {
    /// Load segment and return metadata with REAL size (after processing).
    /// Data is written to AssetStore, not returned in memory.
    async fn load_segment(&self, variant: usize, segment_index: usize) -> HlsResult<SegmentMeta>;

    /// Get number of variants from master playlist.
    fn num_variants(&self) -> usize;

    /// Get total segments in variant's media playlist.
    async fn num_segments(&self, variant: usize) -> HlsResult<usize>;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use url::Url;

    use super::*;
    use crate::cache::SegmentType;

    fn create_test_meta(variant: usize, segment_index: usize, len: u64) -> SegmentMeta {
        SegmentMeta {
            variant,
            segment_type: SegmentType::Media(segment_index),
            sequence: segment_index as u64,
            url: Url::parse(&format!(
                "http://test.com/v{}/seg{}.ts",
                variant, segment_index
            ))
            .expect("valid URL"),
            duration: Some(Duration::from_secs(4)),
            key: None,
            len,
            container: Some(crate::parsing::ContainerFormat::Ts),
        }
    }

    // L-1: MockLoader basic usage
    #[tokio::test]
    async fn test_mock_loader_basic() {
        let mut loader = MockLoader::new();

        loader.expect_num_variants().returning(|| 3);

        loader
            .expect_load_segment()
            .withf(|variant, idx| *variant == 0 && *idx == 5)
            .returning(|variant, idx| Ok(create_test_meta(variant, idx, 200_000)));

        loader
            .expect_num_segments()
            .with(mockall::predicate::eq(0))
            .returning(|_| Ok(100));

        assert_eq!(loader.num_variants(), 3);
        assert_eq!(loader.num_segments(0).await.unwrap(), 100);

        let meta = loader.load_segment(0, 5).await.unwrap();
        assert_eq!(meta.variant, 0);
        assert_eq!(meta.segment_type.media_index(), Some(5));
        assert_eq!(meta.len, 200_000);
    }

    // L-2: MockLoader multiple variants
    #[tokio::test]
    async fn test_mock_loader_multi_variant() {
        let mut loader = MockLoader::new();

        loader.expect_num_variants().returning(|| 3);

        loader.expect_load_segment().returning(|variant, idx| {
            Ok(create_test_meta(
                variant,
                idx,
                200_000 + variant as u64 * 50_000,
            ))
        });

        let meta0 = loader.load_segment(0, 1).await.unwrap();
        let meta1 = loader.load_segment(1, 1).await.unwrap();
        let meta2 = loader.load_segment(2, 1).await.unwrap();

        assert_eq!(meta0.len, 200_000);
        assert_eq!(meta1.len, 250_000);
        assert_eq!(meta2.len, 300_000);
    }
}
