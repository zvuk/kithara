use std::ops::Range;

use kithara_test_utils::kithara;

use super::HlsVariant;

impl HlsVariant {
    pub(crate) fn authoritative_len(&self) -> Option<u64> {
        let total = self.total_bytes();
        (total > 0 && self.sizes_complete()).then_some(total)
    }

    pub(crate) fn eof_ready(&self) -> bool {
        self.sizes_complete() || self.segment_aware_seek_tail_complete()
    }

    /// Reader-facing lookup in **virtual** byte space — delegates to the
    /// [`Layout`], which subtracts `byte_shift`, runs the natural-space
    /// search, and gates against `[served_from..served_until)` under one
    /// lock. Returns `None` when the byte falls outside the served range so
    /// cross-variant lookups in [`HlsCoord::find_at_offset`] fall through to
    /// the previous variant.
    #[kithara::probe(
        variant = self.variant as u64,
        byte_offset,
        found_seg = self
            .layout
            .find_at_offset(byte_offset, &self.segments)
            .map_or(u64::MAX, |(i, _, _)| u64::from(i))
    )]
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        self.seek_alias_at(byte_offset)
            .or_else(|| self.layout.find_at_offset(byte_offset, &self.segments))
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
            #[kithara::probe(
                variant = self.variant as u64,
                total = self.layout.total_bytes()
            )]
            pub(crate) fn total_bytes(&self) -> u64;
        }
    }

    /// Coherent "is this variant historical?" check — `served_from` and
    /// `served_until` read under a single Layout lock.
    pub(crate) fn is_shrunk(&self) -> bool {
        self.layout.is_shrunk(self.num_segments())
    }

    /// True when a same-variant seek needs no layout reset: the offset table
    /// is already the canonical full-range geometry and every served size is
    /// exact, so [`Self::reset_layout_to_full_range`] would reproduce the
    /// identical table. Lets [`HlsCoord::prepare_for_seek`] skip the reset on
    /// a fully-resolved single-variant track. Cross-variant (shifted/shrunk)
    /// or size-incomplete layouts return `false` and keep their reset.
    pub(crate) fn layout_seek_invariant(&self) -> bool {
        self.layout.is_canonical_complete(&self.segments)
    }

    /// Replace the per-variant fetch queue with `[from_seg .. num_segments)`
    /// (plus `Init` if applicable). Does NOT cancel in-flight fetches —
    /// dedup is handled at `dispatch` time via the `Downloading` state.
    /// `dispatch` skips `Downloading` and `Loaded` entries without burning
    /// budget, so the queue can safely include them.
    ///
    /// Cancellation is reserved for variant deactivation
    /// ([`cancel`](Self::cancel) / teardown) — there we really want to
    /// abandon the variant's in-flight work; the freshly activated variant
    /// has its own cancel token. Seek / eviction never need to cancel,
    /// they only need to reseed the queue.
    ///
    /// Callers: seek (`seek_to`), ABR variant flip
    /// (`activate_at_segment`), eviction of an active-variant resource,
    /// and the initial peer activation.
    #[must_use]
    pub(crate) fn num_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
    }

    pub(super) fn reset_layout_to_full_range(&self) {
        self.layout.reset(self.init_route_size(), &self.segments);
    }

    /// Natural byte offset of segment `seg_idx` — i.e. without applying
    /// `byte_shift`. Used internally by `activate_*` to compute the
    /// shift needed to pin a segment at a given virtual byte.
    pub(crate) fn segment_byte_offset_natural(&self, seg_idx: u32) -> Option<u64> {
        self.layout.natural_offset(seg_idx as usize)
    }

    /// Cap the upper bound (exclusive) of segments this variant serves.
    /// Called from [`HlsCoord::commit_variant_switch`] on same-codec ABR
    /// commit so the outgoing variant's `find_at_offset` returns `None`
    /// for segments at or past the boundary — gates the reader's
    /// `SegmentReadStart` events against the post-switch range owned by
    /// the incoming variant, preventing a duplicate `(v_old, from_seg)`
    /// emit when the reader cursor lingers in the boundary segment.
    pub(crate) fn set_served_until(&self, until: u32) {
        self.layout
            .set_served_until(until, &self.segments, self.init_route_size());
    }
}
