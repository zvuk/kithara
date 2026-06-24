use kithara_assets::AssetScope;
use kithara_drm::DecryptContext;

use crate::segment::Segment;

impl Segment {
    /// Store a loaded/committed byte length and mark the size atom EXACT.
    pub(crate) fn set_loaded_size(&self, n: u64) {
        self.size().set_exact(n);
    }

    /// Committed on-disk length for this slot when its resource is `Committed`
    /// with a known `final_len`, routed through the slot's narrow disk handle.
    /// `None` when the resource is not committed.
    pub(crate) fn committed_len(&self, scope: &AssetScope<DecryptContext>) -> Option<u64> {
        self.resource(scope).committed_len()
    }
}
