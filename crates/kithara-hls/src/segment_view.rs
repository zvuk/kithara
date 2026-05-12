//! Lock-free `SegmentLayout` adapter that delegates to the active `HlsVariant`.
//!
//! Plan 04 implements the full module — this file is Pre-Phase 0 skeleton.

#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{Arc, atomic::AtomicUsize},
};

use kithara_platform::time::Duration;
use kithara_stream::{SegmentDescriptor, SegmentLayout};
use kithara_test_utils::kithara;

use crate::variant::HlsVariant;

/// Lock-free `SegmentLayout` impl. Active variant lookup via `Acquire` load
/// on `AtomicUsize`; variant shape (`Arc<Vec<HlsVariant>>`) is immutable.
#[non_exhaustive]
pub struct HlsSegmentView {
    pub variants: Arc<Vec<HlsVariant>>,
    pub active_variant: Arc<AtomicUsize>,
}

impl HlsSegmentView {
    #[must_use]
    pub fn new(variants: Arc<Vec<HlsVariant>>, active_variant: Arc<AtomicUsize>) -> Self {
        Self {
            variants,
            active_variant,
        }
    }
}

impl SegmentLayout for HlsSegmentView {
    #[kithara::probe]
    fn init_segment_range(&self) -> Option<Range<u64>> {
        unimplemented!("Plan 04 — HlsSegmentView::init_segment_range")
    }

    #[kithara::probe]
    fn segment_after_byte(&self, _byte: u64) -> Option<SegmentDescriptor> {
        unimplemented!("Plan 04 — HlsSegmentView::segment_after_byte")
    }

    #[kithara::probe]
    fn segment_at_time(&self, _t: Duration) -> Option<SegmentDescriptor> {
        unimplemented!("Plan 04 — HlsSegmentView::segment_at_time")
    }

    fn segment_count(&self) -> Option<u32> {
        unimplemented!("Plan 04 — HlsSegmentView::segment_count")
    }

    fn len(&self) -> Option<u64> {
        unimplemented!("Plan 04 — HlsSegmentView::len")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_constructs() {
        let view = HlsSegmentView::new(Arc::new(Vec::new()), Arc::new(AtomicUsize::new(0)));
        assert!(view.variants.is_empty());
    }
}
