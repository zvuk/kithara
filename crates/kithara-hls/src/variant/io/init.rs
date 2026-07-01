use std::{ops::Range, sync::Arc};

use kithara_assets::AssetScope;
#[cfg(test)]
use kithara_assets::ResourceKey;
use kithara_drm::DecryptContext;
use kithara_stream::{StreamResult, dl::FetchCmd, needs_exact_byte_sizes};

use super::{HlsVariant, PlanCtx, core::INIT_PLACEHOLDER_BYTES};
use crate::{
    handle::ResourceHandle,
    playlist::{PlaylistAccess, PlaylistState},
    segment::{
        Downloading, FetchClaim, InitSegment, Segment, SegmentContent, SegmentSize,
        SegmentSlotState,
    },
};

impl HlsVariant {
    pub(super) fn build_init_cmd(
        self: &Arc<Self>,
        ctx: &PlanCtx,
        handle: FetchClaim<Downloading>,
    ) -> Option<FetchCmd> {
        let init = self.init()?;
        let resource_handle = self.init_handle()?;
        let resource = resource_handle
            .acquire(init.content())
            .expect("acquire_resource for init must succeed");
        self.build_cmd(
            resource_handle.url().clone(),
            resource,
            handle,
            ctx.signal.clone(),
        )
    }

    /// Builds the variant's init slot. The slot exists (`Some(Segment::Init)`)
    /// iff the playlist carries an `#EXT-X-MAP` URL — NOT iff the init's
    /// byte length is already known (R5). A not-yet-resolved init leaves
    /// `init_size() == 0` while the URL is present; the init is still a real
    /// segment that must be fetched (its committed `final_len` sets the real
    /// size). Keying existence on known size drops such an init, and
    /// `read_at(0)` then serves segment 0's container where the demuxer
    /// expects `ftyp` ("`re_mp4`: ftyp not found") or wedges with no progress.
    /// `None` is the old `VariantInit::NotApplicable`: no `#EXT-X-MAP`, or a
    /// byte-range-embedded init living in segment 0's byte range. See the crate
    /// `CONTEXT.md` "Variant init".
    pub(super) fn build_init_entry(
        playlist_state: &PlaylistState,
        variant_idx: usize,
        decrypt_ctx: Option<DecryptContext>,
        scope: &AssetScope,
    ) -> Option<Segment> {
        let url = playlist_state.init_url(variant_idx)?;
        let needs_exact = needs_exact_byte_sizes(
            playlist_state.variant_codec(variant_idx),
            playlist_state.variant_container(variant_idx),
        );
        let size = if !needs_exact {
            SegmentSize::placeholder(INIT_PLACEHOLDER_BYTES)
        } else {
            SegmentSize::default()
        };
        Some(Segment::Init(InitSegment {
            resource_id: scope.key_from_url(&url),
            url,
            state: SegmentSlotState::missing(),
            size,
            content: SegmentContent::from(decrypt_ctx),
        }))
    }

    /// Whether this variant declares a separately fetched `#EXT-X-MAP` init,
    /// regardless of whether its size is yet known.
    pub(crate) fn has_init(&self) -> bool {
        self.segments.init.is_some()
    }

    /// Whether the declared init settled terminally (`Failed`).
    pub(crate) fn init_failed(&self) -> bool {
        self.segments
            .init
            .as_ref()
            .is_some_and(|seg| seg.state().is_failed())
    }

    /// Resource key for the variant's init segment — `None` when the
    /// playlist has no `#EXT-X-MAP` (raw TS/AAC). Test-only assertion helper;
    /// the reader paths read the init through [`Self::init_handle`].
    #[cfg(test)]
    pub(crate) fn init_resource(&self) -> Option<ResourceKey> {
        Some(self.segments.init.as_ref()?.resource_id().clone())
    }

    pub(crate) fn init_size(&self) -> u64 {
        self.segments.init.as_ref().map_or(0, Segment::read_len)
    }

    pub(super) fn init_route_size(&self) -> u64 {
        self.segments.init.as_ref().map_or(0, Segment::len)
    }

    /// Flip the init slot to `Missing`, clearing the Layout seed when the
    /// variant actually carries an init segment.
    pub(crate) fn invalidate_init(&self) {
        let had_init = self.segments.init.as_ref().is_some_and(|seg| {
            seg.state().mark_missing();
            true
        });
        if had_init {
            self.layout.clear_init_seed();
        }
    }

    /// Whether the next dispatch should issue the separate init fetch
    /// (CMAF `EXT-X-MAP`) — true only if the variant advertises a
    /// non-zero init segment that hasn't been loaded yet.
    pub(super) fn needs_init_fetch(&self) -> bool {
        self.segments
            .init
            .as_ref()
            .is_some_and(|seg| !seg.state().is_loaded())
    }

    /// Whether every byte in `range` is present on disk for the init segment.
    pub(super) fn init_contains(&self, range: Range<u64>) -> bool {
        self.segments
            .init
            .as_ref()
            .is_some_and(|seg| seg.size().is_exact() && seg.contains(&self.segments.scope, range))
    }

    /// Read `range` of the init segment into `dst` via the [`Segment`]
    /// cascade. `Ok(None)` when there is no init or its bytes are not on disk
    /// yet.
    pub(super) fn init_read_at(
        &self,
        range: Range<u64>,
        dst: &mut [u8],
    ) -> StreamResult<Option<usize>> {
        self.segments.init.as_ref().map_or_else(
            || Ok(None),
            |seg| {
                if seg.size().is_exact() {
                    seg.read_at(&self.segments.scope, range, dst)
                } else {
                    Ok(None)
                }
            },
        )
    }

    /// Borrow the init slot — the fetch path matches on it, reading the init
    /// `Segment`'s `url` / `content` / `resource_id` and claiming its state
    /// atom. `None` for a variant with no separate init.
    pub(super) fn init(&self) -> Option<&Segment> {
        self.segments.init.as_ref()
    }

    /// Committed on-disk length of the (separately fetched) init segment, as
    /// [`committed_final_len`](Self::committed_final_len) for media.
    pub(super) fn init_committed_final_len(&self) -> Option<u64> {
        self.segments
            .init
            .as_ref()?
            .committed_len(&self.segments.scope)
    }

    pub(super) fn init_downloading(&self) -> bool {
        self.segments
            .init
            .as_ref()
            .is_some_and(|seg| seg.state().is_downloading())
    }

    /// Narrow disk handle for the variant's separately fetched init segment,
    /// or `None` for a variant with no `#EXT-X-MAP` init.
    fn init_handle(&self) -> Option<ResourceHandle> {
        Some(self.segments.init.as_ref()?.resource(&self.segments.scope))
    }
}
