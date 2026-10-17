use std::ops::Range;

use kithara_bufpool::HasPool;
use kithara_stream::needs_exact_byte_sizes;
use kithara_test_utils::kithara;
use tracing::debug;

use crate::{segment::PlannedFetch, variant::HlsVariant};

impl<S> HlsVariant<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    #[kithara::probe(variant = self.variant as u64, natural_total = self.layout.total_bytes())]
    pub(crate) fn total_bytes(&self) -> u64 {
        let total = self.layout.total_bytes();
        if !needs_exact_byte_sizes(self.profile.codec, self.profile.container)
            && let Some(alias) = self.seek.alias.load()
            && let Some(offset) = self.layout.segment_byte_offset(alias.segment)
        {
            return alias.anchor.saturating_add(total.saturating_sub(offset));
        }
        total
    }

    pub(crate) fn authoritative_len(&self) -> Option<u64> {
        self.layout.try_published(|| {
            let total = self.total_bytes();
            (total > 0 && self.sizes_complete()).then_some(total)
        })
    }

    pub(crate) fn eof_at(&self, offset: u64) -> bool {
        self.eof_at_with(offset, || {})
    }

    /// Logs once per stream: which geometry the offset was judged against is the one fact worth
    /// having when a track ends early.
    pub(in crate::variant) fn eof_at_published(&self, offset: u64, total: u64) -> bool {
        let eof = total > 0 && offset >= total && self.eof_ready();
        if eof {
            debug!(
                variant = self.variant,
                offset,
                total,
                served_from = self.served_from(),
                segments = self.num_segments(),
                sizes_complete = self.sizes_complete(),
                "minting byte EOF"
            );
        }
        eof
    }

    fn eof_at_with(&self, offset: u64, before_ready: impl FnOnce()) -> bool {
        self.layout
            .try_published(|| {
                let total = self.total_bytes();
                before_ready();
                Some(self.eof_at_published(offset, total))
            })
            .unwrap_or(false)
    }

    pub(crate) fn eof_ready(&self) -> bool {
        self.sizes_complete() || self.segment_aware_seek_tail_complete()
    }

    /// Reader-facing lookup in **virtual** byte space. A live seek alias
    /// answers first; otherwise the [`Layout`] subtracts `byte_shift`, runs
    /// the natural-space search and gates against `[served_from..served_until)`,
    /// returning `None` outside the served range so cross-variant lookups fall
    /// through to the previous variant.
    ///
    /// The probe mirrors the body rather than re-running the table arm alone:
    /// a byte an alias answers resolves elsewhere in the table, so the table's
    /// answer would name a segment this call never returned. `is_aliased` says
    /// which arm answered, and `served_from` names the table's frame — a
    /// re-mint between a fetch plan and a later wait moves every byte.
    #[kithara::probe(
        variant = self.variant as u64,
        byte_offset,
        found_seg = self
            .seek_alias_at(byte_offset)
            .or_else(|| self.layout.find_at_offset(byte_offset, &self.segments))
            .map_or(u64::MAX, |(i, _, _)| u64::from(i)),
        is_aliased = self.seek_alias_at(byte_offset).is_some(),
        served_from = u64::from(self.served_from())
    )]
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        self.seek_alias_at(byte_offset)
            .or_else(|| self.layout.find_at_offset(byte_offset, &self.segments))
    }

    /// Whether full-range, exact geometry has no deferred prefix and needs no layout re-mint.
    /// A live seek tail alone changes nothing, but even an already-exact size can have a parked revision.
    /// Gates seek reset and [`Self::reset_layout_to_full_range`]; locks deferred state, so off-RT only.
    pub(crate) fn layout_seek_invariant(&self) -> bool {
        self.layout.is_canonical_complete(&self.segments)
            && self.seek.deferred_prefix.lock().is_empty()
    }

    /// Number of media segments in this variant, saturated to `u32::MAX`.
    #[must_use]
    pub(crate) fn num_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
    }

    /// Resets under the layout's write lock so the seek tail's freeze retirement and the sizes
    /// parked behind it land atomically with the fresh frame.
    pub(in crate::variant) fn reset_layout_to_full_range(&self) {
        if self.layout_seek_invariant() {
            return;
        }
        self.layout.reset(&self.segments, || {
            self.clear_segment_aware_seek_tail();
            for (idx, len) in self.seek.deferred_prefix.lock().drain(..) {
                self.apply_loaded_size(PlannedFetch::Segment(idx), len);
            }
            self.init_route_size()
        });
    }

    delegate::delegate! {
        to self {
            /// Init segment range in **natural** byte space — always
            /// `0..init_size`, regardless of post-commit `served_from`. Returns
            /// an empty range (`0..0`) when the variant has no `#EXT-X-MAP`
            /// init (raw TS/AAC/MPEG-ES).
            ///
            /// The "is this init addressable in the merged virtual space?"
            /// question lives in the *caller* (e.g. `init_descriptor_at`) which
            /// combines this with `served_from()` — keeping virtual-space
            /// concerns out of a per-variant primitive avoids silently dropping
            /// post-commit inits at the `ByteMap` boundary.
            #[kithara::probe(variant = self.variant as u64, size = self.init_size())]
            #[expr(0..$)]
            #[call(init_size)]
            pub(crate) fn init_byte_range(&self) -> Range<u64>;
            #[call(authoritative_len)]
            pub(crate) fn stream_len(&self) -> Option<u64>;
            #[cfg(test)]
            #[call(eof_at_with)]
            pub(crate) fn eof_at_before_ready_check(
                &self,
                offset: u64,
                before_ready: impl FnOnce(),
            ) -> bool;
        }
        to self.layout {
            /// Virtual byte offset of segment `seg_idx` in the combined stream.
            /// For the initial variant (`byte_shift == 0`) this equals the natural
            /// offset; after an Auto-mode switch this places the segment relative
            /// to the reader's current byte position at the switch boundary.
            pub(crate) fn segment_byte_offset(&self, seg_idx: u32) -> Option<u64>;
            pub(crate) fn served_from(&self) -> u32;
            /// Whether every served segment's byte size is known. While `false`,
            /// [`Self::total_bytes`] is a lower bound (a segment's size estimate is
            /// missing), so the byte-EOF gates must hold `Waiting`/`Pending` rather
            /// than mint EOF for an in-range offset that only looks past-the-end
            /// against the under-count.
            pub(crate) fn sizes_complete(&self) -> bool;
        }
    }
}
