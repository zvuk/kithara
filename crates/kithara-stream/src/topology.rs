#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_platform::time::Duration;

use crate::{MediaInfo, SourceSeekAnchor};

pub trait Topology: Send + Sync + 'static {
    fn num_variants(&self) -> usize {
        1
    }

    fn num_segments(&self, variant: usize) -> Option<usize>;

    fn media_info(&self, variant: usize) -> Option<MediaInfo> {
        let _ = variant;
        None
    }

    fn seek_anchor(&self, variant: usize, target: Duration) -> Option<SourceSeekAnchor> {
        let _ = (variant, target);
        None
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }

    fn total_len(&self, variant: usize) -> Option<u64> {
        let _ = variant;
        None
    }
}

impl Topology for () {
    fn num_segments(&self, variant: usize) -> Option<usize> {
        (variant == 0).then_some(1)
    }
}

impl<T> Topology for Arc<T>
where
    T: Topology + ?Sized,
{
    fn num_variants(&self) -> usize {
        (**self).num_variants()
    }

    fn num_segments(&self, variant: usize) -> Option<usize> {
        (**self).num_segments(variant)
    }

    fn media_info(&self, variant: usize) -> Option<MediaInfo> {
        (**self).media_info(variant)
    }

    fn seek_anchor(&self, variant: usize, target: Duration) -> Option<SourceSeekAnchor> {
        (**self).seek_anchor(variant, target)
    }

    fn total_duration(&self) -> Option<Duration> {
        (**self).total_duration()
    }

    fn total_len(&self, variant: usize) -> Option<u64> {
        (**self).total_len(variant)
    }
}
