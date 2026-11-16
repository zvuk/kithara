use std::sync::atomic::Ordering;

use kithara_bufpool::HasPool;
use tracing::debug;

use crate::{
    segment::{FetchClaim, Loaded, PlannedFetch},
    variant::HlsVariant,
};

impl<S> HlsVariant<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Settle hook: shrinks the appropriate size atom to `actual` and
    /// rebuilds the offset map. Called from
    /// [`FetchSlot::settle`] via `Weak<HlsVariant>::upgrade()` once the
    /// resource commits — for DRM, this is where the post-PKCS7 length
    /// replaces the encrypted estimate.
    ///
    /// Size store and offset recompute happen under the same Layout write
    /// lock — a reader that races in between would see a new size with
    /// stale offsets and fall into a non-existent gap, hanging on
    /// `range_ready`. The closure performs the caller-owned size store and
    /// reports the post-store `init_size` to seed the recompute.
    pub(crate) fn apply_commit(&self, loaded: &FetchClaim<Loaded, S>) {
        self.layout.apply_commit(&self.segments, || {
            if !self.defer_prefix_settle(loaded.planned(), loaded.final_len()) {
                self.apply_loaded_size(loaded.planned(), loaded.final_len());
            }
            self.init_route_size()
        });
        self.complete_exact_seek_if_ready();
    }

    /// Settle-side size store: shrink the appropriate atom to `final_len`.
    /// The caller runs this inside [`Layout::apply_commit`](
    /// crate::variant::map::offsets::Layout::apply_commit)'s write-lock so a reader never
    /// observes a new size against a stale offset table.
    ///
    /// Settling a `None` init slot is a no-op: only a `Some(Init)` slot is ever fetched, so a stray
    /// settle cannot resurrect an absent init entry.
    pub(in crate::variant) fn apply_loaded_size(&self, planned: PlannedFetch, final_len: u64) {
        match planned {
            PlannedFetch::Init => {
                if let Some(init) = self.segments.init.as_ref() {
                    init.set_loaded_size(final_len);
                }
            }
            PlannedFetch::Segment(idx) => {
                if let Some(slot) = self.segments.get(idx as usize) {
                    slot.set_loaded_size(final_len);
                }
            }
        }
    }

    /// Post-seek frame stability: while a segment-aware seek tail is
    /// active, the reader, the demuxer's byte map, and the peer's cursor
    /// all hold bytes minted on the frame the seek anchored, so a settle
    /// for a media segment behind the tail must not re-key that space —
    /// its size parks in `deferred_prefix` and the next space re-mint
    /// applies it. The init is never parked: init reads gate on its size
    /// being exact, so a parked init starves every consumer that still
    /// needs its bytes (the demuxer probe of an ABR pending variant
    /// deadlocks before activation can drain it). Runs inside the
    /// [`Layout`] write lock, so the freeze decision cannot race a seek
    /// re-minting the space.
    pub(in crate::variant) fn defer_prefix_settle(
        &self,
        planned: PlannedFetch,
        final_len: u64,
    ) -> bool {
        let tail = self.seek.segment_aware_tail.load(Ordering::Acquire);
        if tail == Self::NO_SEEK_TAIL {
            return false;
        }
        let PlannedFetch::Segment(idx) = planned else {
            return false;
        };
        if idx >= tail {
            return false;
        }
        debug!(
            variant = self.variant,
            segment = idx,
            final_len,
            tail,
            "parking a settle behind the live seek tail"
        );
        self.seek.deferred_prefix.lock().push((idx, final_len));
        true
    }
}
