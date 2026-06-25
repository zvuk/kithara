use kithara_assets::ResourceKey;
use kithara_test_utils::kithara;

use super::HlsVariant;

impl HlsVariant {
    /// Returns evicted `seg_idx` (`-1` for init), or `None` if `key` doesn't belong to this variant.
    /// State flips `Loaded -> Missing`; queue reseeding is the caller's job
    /// (see `HlsCoord::broadcast_eviction` → `rebuild` for the active
    /// variant; non-active variants' queues are rebuilt lazily on the
    /// next ABR flip).
    #[kithara::probe(variant = self.variant as u64)]
    pub(crate) fn on_evict(&self, key: &ResourceKey) -> Option<i32> {
        if let Some(init) = self.segments.init.as_ref()
            && init.resource_id() == key
        {
            init.state().mark_missing();
            return Some(-1);
        }
        let (seg_idx, seg) = self
            .segments
            .iter()
            .enumerate()
            .find(|(_, seg)| seg.resource_id() == key)?;
        seg.state().mark_missing();
        i32::try_from(seg_idx).ok()
    }
}
